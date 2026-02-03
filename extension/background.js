/**
 * Background Service Worker for Qontinui DevTools Extension
 *
 * Handles communication between popup and content scripts,
 * and sends captured DOM to the qontinui-runner API.
 * Also provides API request recording and UI Bridge inspection functionality.
 */

const RUNNER_API = "http://localhost:9876";
const RUNNER_WS_URL = "ws://localhost:9876/ws/extension";

// =============================================================================
// WebSocket Connection to Runner (for exploration commands)
// =============================================================================

let wsConnection = null;
let wsReconnectTimeout = null;
let wsConnected = false;
let wsPendingRequests = new Map(); // requestId -> { resolve, reject, timeout }

// Selected tab for exploration (null = use active tab)
let selectedExplorationTabId = null;
let selectedExplorationTabInfo = null; // { id, url, title } for display

// Load persisted selected tab on startup
chrome.storage.local.get(['selectedExplorationTabId', 'selectedExplorationTabInfo'], (result) => {
  if (result.selectedExplorationTabId) {
    selectedExplorationTabId = result.selectedExplorationTabId;
    selectedExplorationTabInfo = result.selectedExplorationTabInfo || null;
    console.log("[Qontinui] Restored selected tab from storage:", selectedExplorationTabId);
    // Verify tab still exists
    chrome.tabs.get(selectedExplorationTabId).then((tab) => {
      if (tab) {
        selectedExplorationTabInfo = { id: tab.id, url: tab.url, title: tab.title };
      }
    }).catch(() => {
      // Tab no longer exists, clear selection
      console.log("[Qontinui] Previously selected tab no longer exists, clearing");
      selectedExplorationTabId = null;
      selectedExplorationTabInfo = null;
      chrome.storage.local.remove(['selectedExplorationTabId', 'selectedExplorationTabInfo']);
    });
  }
});

/**
 * Save selected tab to storage for persistence
 */
function persistSelectedTab() {
  if (selectedExplorationTabId !== null) {
    chrome.storage.local.set({
      selectedExplorationTabId,
      selectedExplorationTabInfo
    });
  } else {
    chrome.storage.local.remove(['selectedExplorationTabId', 'selectedExplorationTabInfo']);
  }
}

// Recording state
let activeRecordingSession = null; // { tabId, startTime, snapshots: [] }

// =============================================================================
// Capture Session Management (for State Machine Discovery)
// =============================================================================

/**
 * Active capture session for state machine discovery.
 * Tracks element fingerprints across multiple page captures.
 */
let activeCaptureSession = null;
/* Structure:
{
  sessionId: string,           // UUID for the session
  startedAt: number,           // Timestamp when session started
  captures: [                  // Array of capture records
    {
      captureId: string,       // UUID for this capture
      timestamp: number,
      url: string,
      title: string,
      elementFingerprints: string[],  // Array of fingerprint hashes
      triggeredBy?: {          // If this capture followed an action
        actionType: string,
        targetFingerprint: string,
        previousCaptureId: string
      }
    }
  ],
  actions: [                   // Array of action records
    {
      actionId: string,
      timestamp: number,
      actionType: string,
      targetFingerprint: string,
      beforeCaptureId: string,
      afterCaptureId: string,
      addedFingerprints: string[],
      removedFingerprints: string[]
    }
  ],
  fingerprintCatalog: Map<string, object>  // hash -> full fingerprint object
}
*/

/**
 * Generate a UUID v4
 */
function generateUUID() {
  return 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, function(c) {
    const r = Math.random() * 16 | 0;
    const v = c === 'x' ? r : (r & 0x3 | 0x8);
    return v.toString(16);
  });
}

/**
 * Start a new capture session
 */
function startCaptureSession() {
  const sessionId = generateUUID();
  activeCaptureSession = {
    sessionId,
    startedAt: Date.now(),
    captures: [],
    actions: [],
    fingerprintCatalog: new Map()
  };
  console.log("[Qontinui] Started capture session:", sessionId);
  return { sessionId, startedAt: activeCaptureSession.startedAt };
}

/**
 * End the current capture session
 */
function endCaptureSession() {
  if (!activeCaptureSession) {
    return null;
  }

  const session = {
    ...activeCaptureSession,
    endedAt: Date.now(),
    // Convert Map to object for serialization
    fingerprintCatalog: Object.fromEntries(activeCaptureSession.fingerprintCatalog)
  };

  console.log("[Qontinui] Ended capture session:", session.sessionId,
    "with", session.captures.length, "captures and", session.actions.length, "actions");

  activeCaptureSession = null;
  return session;
}

/**
 * Create a capture record from element data
 */
function createCaptureRecord(url, title, elements, triggeredBy = null) {
  if (!activeCaptureSession) {
    // Auto-start a session if none exists
    startCaptureSession();
  }

  const captureId = generateUUID();
  const timestamp = Date.now();

  // Extract fingerprint hashes and update catalog
  const elementFingerprints = [];
  for (const el of elements) {
    if (el.fingerprint && el.fingerprint.hash) {
      elementFingerprints.push(el.fingerprint.hash);
      // Add to catalog if not already present
      if (!activeCaptureSession.fingerprintCatalog.has(el.fingerprint.hash)) {
        activeCaptureSession.fingerprintCatalog.set(el.fingerprint.hash, el.fingerprint);
      }
    }
  }

  const captureRecord = {
    captureId,
    timestamp,
    url,
    title,
    elementFingerprints,
    elementCount: elements.length,
    triggeredBy: triggeredBy || undefined
  };

  activeCaptureSession.captures.push(captureRecord);
  console.log("[Qontinui] Created capture:", captureId, "with", elementFingerprints.length, "fingerprints");

  return captureRecord;
}

/**
 * Record an action and compute the state change
 */
function recordAction(actionType, targetFingerprint, beforeCaptureId, afterCaptureId) {
  if (!activeCaptureSession) {
    console.warn("[Qontinui] No active session for action recording");
    return null;
  }

  const actionId = generateUUID();

  // Find the before and after captures
  const beforeCapture = activeCaptureSession.captures.find(c => c.captureId === beforeCaptureId);
  const afterCapture = activeCaptureSession.captures.find(c => c.captureId === afterCaptureId);

  if (!beforeCapture || !afterCapture) {
    console.warn("[Qontinui] Could not find before/after captures for action");
    return null;
  }

  // Compute fingerprint diff
  const beforeSet = new Set(beforeCapture.elementFingerprints);
  const afterSet = new Set(afterCapture.elementFingerprints);

  const addedFingerprints = [...afterSet].filter(fp => !beforeSet.has(fp));
  const removedFingerprints = [...beforeSet].filter(fp => !afterSet.has(fp));

  const actionRecord = {
    actionId,
    timestamp: Date.now(),
    actionType,
    targetFingerprint,
    beforeCaptureId,
    afterCaptureId,
    addedFingerprints,
    removedFingerprints
  };

  activeCaptureSession.actions.push(actionRecord);
  console.log("[Qontinui] Recorded action:", actionType,
    "added:", addedFingerprints.length, "removed:", removedFingerprints.length);

  return actionRecord;
}

/**
 * Get current session status
 */
function getCaptureSessionStatus() {
  if (!activeCaptureSession) {
    return { active: false };
  }

  return {
    active: true,
    sessionId: activeCaptureSession.sessionId,
    startedAt: activeCaptureSession.startedAt,
    captureCount: activeCaptureSession.captures.length,
    actionCount: activeCaptureSession.actions.length,
    uniqueFingerprints: activeCaptureSession.fingerprintCatalog.size
  };
}

/**
 * Connect to the runner's WebSocket endpoint
 * Only call this after verifying the runner is available via health check
 */
function connectWebSocket() {
  if (wsConnection && (wsConnection.readyState === WebSocket.CONNECTING || wsConnection.readyState === WebSocket.OPEN)) {
    return;
  }

  console.log("[Qontinui] Connecting to runner WebSocket:", RUNNER_WS_URL);

  try {
    wsConnection = new WebSocket(RUNNER_WS_URL);

    wsConnection.onopen = () => {
      console.log("[Qontinui] WebSocket connected to runner");
      wsConnected = true;
      // Clear any pending reconnect
      if (wsReconnectTimeout) {
        clearTimeout(wsReconnectTimeout);
        wsReconnectTimeout = null;
      }
    };

    wsConnection.onclose = (event) => {
      console.log("[Qontinui] WebSocket disconnected:", event.code, event.reason);
      wsConnected = false;
      wsConnection = null;
      // Reject all pending requests
      for (const [_requestId, pending] of wsPendingRequests.entries()) {
        clearTimeout(pending.timeout);
        pending.reject(new Error("WebSocket disconnected"));
      }
      wsPendingRequests.clear();
      // Schedule reconnection (will check health first)
      scheduleReconnect();
    };

    wsConnection.onerror = () => {
      // Don't log - Chrome already logs WebSocket errors
      // This is expected when runner disconnects
      wsConnected = false;
    };

    wsConnection.onmessage = (event) => {
      try {
        const message = JSON.parse(event.data);
        handleWebSocketMessage(message);
      } catch (e) {
        console.error("[Qontinui] Failed to parse WebSocket message:", e);
      }
    };
  } catch {
    // Silently schedule reconnect - don't log errors for expected failures
    scheduleReconnect();
  }
}

/**
 * Schedule WebSocket reconnection
 * Checks if runner is available before attempting to connect
 */
function scheduleReconnect() {
  if (wsReconnectTimeout) {
    return; // Already scheduled
  }
  wsReconnectTimeout = setTimeout(() => {
    wsReconnectTimeout = null;
    // Check if runner is available before trying to connect
    fetch(`${RUNNER_API}/health`, { method: "GET" })
      .then((response) => {
        if (response.ok) {
          connectWebSocket();
        } else {
          scheduleReconnect();
        }
      })
      .catch(() => {
        // Runner not available, try again later
        scheduleReconnect();
      });
  }, 5000);
}

/**
 * Handle incoming WebSocket messages from runner
 */
async function handleWebSocketMessage(message) {
  const { type, requestId, action, params } = message;

  if (type === "EXPLORATION_COMMAND") {
    // Runner is sending an exploration command to execute via the extension
    console.log("[Qontinui] Received exploration command:", action, requestId);
    try {
      const result = await executeExplorationCommand(action, params || {});
      sendWebSocketResponse(requestId, true, result);
    } catch (error) {
      console.error("[Qontinui] Exploration command failed:", error);
      sendWebSocketResponse(requestId, false, null, error.message);
    }
  } else if (type === "EXPLORATION_RESPONSE") {
    // Response to a request we sent to the runner
    const pending = wsPendingRequests.get(requestId);
    if (pending) {
      clearTimeout(pending.timeout);
      wsPendingRequests.delete(requestId);
      if (message.success) {
        pending.resolve(message.data);
      } else {
        pending.reject(new Error(message.error || "Unknown error"));
      }
    }
  } else if (type === "PING") {
    // Respond to ping with pong
    sendWebSocketMessage({ type: "PONG", requestId });
  }
}

/**
 * Get the target tab for exploration commands.
 * Uses selectedExplorationTabId if set, otherwise falls back to active tab.
 */
async function getTargetTab() {
  // If a specific tab is selected, use it
  if (selectedExplorationTabId !== null) {
    try {
      const tab = await chrome.tabs.get(selectedExplorationTabId);
      if (tab) {
        return tab;
      }
    } catch {
      // Tab no longer exists, clear selection
      console.log("[Qontinui] Selected tab no longer exists, clearing selection");
      selectedExplorationTabId = null;
      selectedExplorationTabInfo = null;
      persistSelectedTab();
    }
  }

  // Fall back to active tab
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  return tab;
}

/**
 * Execute an exploration command by forwarding to the target tab's content script
 */
async function executeExplorationCommand(action, params) {
  // Handle tab management actions first (don't need a target tab)
  switch (action) {
    case "listTabs": {
      // List all tabs that could be explored
      const allTabs = await chrome.tabs.query({});
      return {
        tabs: allTabs
          .filter(t => t.url && !t.url.startsWith("chrome://") && !t.url.startsWith("chrome-extension://"))
          .map(t => ({
            id: t.id,
            url: t.url,
            title: t.title,
            active: t.active,
            windowId: t.windowId,
            favIconUrl: t.favIconUrl,
          })),
        selectedTabId: selectedExplorationTabId,
      };
    }

    case "selectTab":
      // Select a specific tab for exploration
      console.log("[Qontinui BG] selectTab called with params:", params);
      if (params.tabId !== undefined && params.tabId !== null) {
        try {
          const tab = await chrome.tabs.get(params.tabId);
          if (tab) {
            selectedExplorationTabId = params.tabId;
            selectedExplorationTabInfo = { id: tab.id, url: tab.url, title: tab.title };
            persistSelectedTab();
            console.log("[Qontinui BG] Selected tab for exploration:", tab.url, "tabInfo:", selectedExplorationTabInfo);
            return { success: true, tabId: params.tabId, url: tab.url, title: tab.title };
          }
        } catch {
          throw new Error(`Tab ${params.tabId} not found`);
        }
      }
      throw new Error("tabId is required");

    case "clearSelectedTab":
      // Clear tab selection (will use active tab)
      selectedExplorationTabId = null;
      selectedExplorationTabInfo = null;
      persistSelectedTab();
      console.log("[Qontinui] Cleared tab selection, will use active tab");
      return { success: true, selectedTabId: null };

    case "getSelectedTab":
      // Get the currently selected tab info (for popup display)
      // Verify the tab still exists and update info
      if (selectedExplorationTabId !== null) {
        try {
          const tab = await chrome.tabs.get(selectedExplorationTabId);
          selectedExplorationTabInfo = { id: tab.id, url: tab.url, title: tab.title };
          return {
            selectedTabId: selectedExplorationTabId,
            selectedTabInfo: selectedExplorationTabInfo
          };
        } catch {
          // Tab no longer exists
          selectedExplorationTabId = null;
          selectedExplorationTabInfo = null;
          persistSelectedTab();
          return { selectedTabId: null, selectedTabInfo: null };
        }
      }
      return { selectedTabId: null, selectedTabInfo: null };

    case "getActiveTab": {
      // Get currently active/selected tab info (for page capture)
      const targetTab = await getTargetTab();
      if (targetTab) {
        return { url: targetTab.url, title: targetTab.title, id: targetTab.id };
      }
      throw new Error("No active tab found");
    }

    case "capturePageScreenshot": {
      // Capture visible tab screenshot for thumbnail generation.
      // Optionally scrolls an element into view before capturing.
      //
      // Parameters:
      //   - scrollToElement: If true, scroll the element into view before capturing
      //   - elementBounds: { x, y, width, height } - Bounds of the element to scroll into view
      //   - viewportHeight: Current viewport height (used to determine if scroll is needed)
      //   - restoreScroll: If true (default), restore original scroll position after capture
      //
      const targetTab = await getTargetTab();
      if (!targetTab || !targetTab.id) {
        throw new Error("No target tab available for screenshot capture");
      }

      try {
        // Focus the tab's window to ensure we capture the right content
        await chrome.windows.update(targetTab.windowId, { focused: true });
        // Small delay to ensure window is focused
        await new Promise(r => setTimeout(r, 50));

        // If scroll-based capture is requested, scroll element into view
        const scrollToElement = params?.scrollToElement === true;
        const elementBounds = params?.elementBounds;
        const restoreScroll = params?.restoreScroll !== false; // Default to true

        let scrollInfo = null;

        if (scrollToElement && elementBounds) {
          // Execute scroll in content script
          try {
            const scrollResults = await chrome.scripting.executeScript({
              target: { tabId: targetTab.id },
              func: (bounds, _shouldRestore) => {
                // Save current scroll position
                const originalScrollX = window.scrollX;
                const originalScrollY = window.scrollY;
                const viewportHeight = window.innerHeight;
                const viewportWidth = window.innerWidth;

                // Calculate if element is outside viewport
                const elementTop = bounds.y;
                const elementBottom = bounds.y + bounds.height;
                const elementLeft = bounds.x;
                const elementRight = bounds.x + bounds.width;

                const isAboveViewport = elementBottom < 0;
                const isBelowViewport = elementTop > viewportHeight;
                const isLeftOfViewport = elementRight < 0;
                const isRightOfViewport = elementLeft > viewportWidth;

                const needsScroll = isAboveViewport || isBelowViewport || isLeftOfViewport || isRightOfViewport;

                if (needsScroll) {
                  // Calculate scroll position to center the element in viewport
                  const targetScrollY = Math.max(0, originalScrollY + elementTop - (viewportHeight / 2) + (bounds.height / 2));
                  const targetScrollX = Math.max(0, originalScrollX + elementLeft - (viewportWidth / 2) + (bounds.width / 2));

                  window.scrollTo({
                    left: targetScrollX,
                    top: targetScrollY,
                    behavior: 'instant'
                  });

                  // Calculate new element position after scroll
                  const scrollDeltaX = window.scrollX - originalScrollX;
                  const scrollDeltaY = window.scrollY - originalScrollY;

                  return {
                    scrolled: true,
                    originalScrollX,
                    originalScrollY,
                    newScrollX: window.scrollX,
                    newScrollY: window.scrollY,
                    scrollDeltaX,
                    scrollDeltaY,
                    // New bounds relative to the new scroll position
                    newBounds: {
                      x: bounds.x - scrollDeltaX,
                      y: bounds.y - scrollDeltaY,
                      width: bounds.width,
                      height: bounds.height
                    },
                    viewportWidth,
                    viewportHeight
                  };
                }

                return {
                  scrolled: false,
                  originalScrollX,
                  originalScrollY,
                  viewportWidth,
                  viewportHeight
                };
              },
              args: [elementBounds, restoreScroll]
            });

            if (scrollResults && scrollResults[0]?.result) {
              scrollInfo = scrollResults[0].result;

              if (scrollInfo.scrolled) {
                // Wait for scroll to complete and page to render
                await new Promise(r => setTimeout(r, 100));
              }
            }
          } catch (scrollError) {
            console.warn("[Qontinui] Scroll failed, capturing anyway:", scrollError.message);
          }
        }

        // Capture the screenshot
        const dataUrl = await chrome.tabs.captureVisibleTab(targetTab.windowId, { format: "png" });
        // Extract base64 from data URL (remove "data:image/png;base64," prefix)
        const base64 = dataUrl.split(",")[1];

        // Restore scroll position if we scrolled and restoreScroll is true
        if (scrollInfo?.scrolled && restoreScroll) {
          try {
            await chrome.scripting.executeScript({
              target: { tabId: targetTab.id },
              func: (originalX, originalY) => {
                window.scrollTo({
                  left: originalX,
                  top: originalY,
                  behavior: 'instant'
                });
              },
              args: [scrollInfo.originalScrollX, scrollInfo.originalScrollY]
            });
          } catch (restoreError) {
            console.warn("[Qontinui] Failed to restore scroll position:", restoreError.message);
          }
        }

        return {
          screenshot: base64,
          capturedAt: Date.now(),
          viewport: {
            width: scrollInfo?.viewportWidth || targetTab.width || 0,
            height: scrollInfo?.viewportHeight || targetTab.height || 0
          },
          tabId: targetTab.id,
          url: targetTab.url,
          // Include scroll info so caller knows if/how the element was scrolled
          scrollInfo: scrollInfo ? {
            scrolled: scrollInfo.scrolled,
            newBounds: scrollInfo.newBounds || null
          } : null
        };
      } catch (e) {
        console.error("[Qontinui] Screenshot capture failed:", e.message);
        throw new Error(`Screenshot capture failed: ${e.message}`);
      }
    }

    case "captureFullPageScreenshot": {
      // Capture the entire page by scrolling and capturing viewport-sized tiles.
      // Returns an array of tile screenshots with their positions for client-side stitching.
      //
      // Parameters:
      //   - tileDelay: Delay in ms between tile captures (default: 150)
      //   - hideFixedElements: If true, hide fixed/sticky elements during capture (default: false)
      //   - progressRequestId: Optional request ID for progress updates via WebSocket
      //
      // Returns:
      //   - tiles: Array of { screenshot, x, y, width, height } tile data
      //   - totalWidth: Full page width
      //   - totalHeight: Full page height
      //   - viewportWidth: Viewport width used for tiles
      //   - viewportHeight: Viewport height used for tiles
      //
      const targetTab = await getTargetTab();
      if (!targetTab || !targetTab.id) {
        throw new Error("No target tab available for full page screenshot");
      }

      const tileDelay = params?.tileDelay ?? 150;
      const hideFixedElements = params?.hideFixedElements ?? false;
      const progressRequestId = params?.progressRequestId ?? null;

      // Helper to send progress updates via WebSocket
      const sendProgress = (currentTile, totalTiles, phase) => {
        if (progressRequestId && wsConnected) {
          sendWebSocketMessage({
            type: "FULL_PAGE_CAPTURE_PROGRESS",
            requestId: progressRequestId,
            progress: {
              currentTile,
              totalTiles,
              phase
            }
          });
        }
      };

      try {
        // Focus the tab's window
        await chrome.windows.update(targetTab.windowId, { focused: true });
        await new Promise(r => setTimeout(r, 50));

        // Get page dimensions and initial scroll position
        const dimensionResults = await chrome.scripting.executeScript({
          target: { tabId: targetTab.id },
          func: (shouldHideFixed) => {
            // Get full page dimensions
            const totalWidth = Math.max(
              document.body.scrollWidth,
              document.documentElement.scrollWidth,
              document.body.offsetWidth,
              document.documentElement.offsetWidth,
              document.body.clientWidth,
              document.documentElement.clientWidth
            );
            const totalHeight = Math.max(
              document.body.scrollHeight,
              document.documentElement.scrollHeight,
              document.body.offsetHeight,
              document.documentElement.offsetHeight,
              document.body.clientHeight,
              document.documentElement.clientHeight
            );

            const viewportWidth = window.innerWidth;
            const viewportHeight = window.innerHeight;

            // Save original scroll position
            const originalScrollX = window.scrollX;
            const originalScrollY = window.scrollY;

            // Optionally hide fixed/sticky elements
            let hiddenElements = [];
            if (shouldHideFixed) {
              const allElements = document.querySelectorAll('*');
              allElements.forEach(el => {
                const style = window.getComputedStyle(el);
                if (style.position === 'fixed' || style.position === 'sticky') {
                  hiddenElements.push({
                    element: el,
                    originalVisibility: el.style.visibility
                  });
                  el.style.visibility = 'hidden';
                }
              });
            }

            return {
              totalWidth,
              totalHeight,
              viewportWidth,
              viewportHeight,
              originalScrollX,
              originalScrollY,
              hiddenCount: hiddenElements.length
            };
          },
          args: [hideFixedElements]
        });

        if (!dimensionResults || !dimensionResults[0]?.result) {
          throw new Error("Failed to get page dimensions");
        }

        const {
          totalWidth,
          totalHeight,
          viewportWidth,
          viewportHeight,
          originalScrollX,
          originalScrollY
        } = dimensionResults[0].result;

        console.log(`[Qontinui] Full page capture: ${totalWidth}x${totalHeight}, viewport: ${viewportWidth}x${viewportHeight}`);

        // Calculate tile grid
        const numCols = Math.ceil(totalWidth / viewportWidth);
        const numRows = Math.ceil(totalHeight / viewportHeight);
        const totalTiles = numCols * numRows;

        console.log(`[Qontinui] Capturing ${totalTiles} tiles (${numCols}x${numRows})`);

        // Send initial progress
        sendProgress(0, totalTiles, 'capturing');

        const tiles = [];

        // Capture each tile
        for (let row = 0; row < numRows; row++) {
          for (let col = 0; col < numCols; col++) {
            const scrollX = col * viewportWidth;
            const scrollY = row * viewportHeight;

            // Scroll to tile position
            await chrome.scripting.executeScript({
              target: { tabId: targetTab.id },
              func: (x, y) => {
                window.scrollTo({
                  left: x,
                  top: y,
                  behavior: 'instant'
                });
              },
              args: [scrollX, scrollY]
            });

            // Wait for scroll to complete and content to render
            await new Promise(r => setTimeout(r, tileDelay));

            // Get actual scroll position (may differ if we hit page boundaries)
            const scrollPosResults = await chrome.scripting.executeScript({
              target: { tabId: targetTab.id },
              func: () => ({
                actualScrollX: window.scrollX,
                actualScrollY: window.scrollY
              })
            });

            const actualScrollX = scrollPosResults?.[0]?.result?.actualScrollX ?? scrollX;
            const actualScrollY = scrollPosResults?.[0]?.result?.actualScrollY ?? scrollY;

            // Capture the visible viewport
            const dataUrl = await chrome.tabs.captureVisibleTab(targetTab.windowId, { format: "png" });
            const base64 = dataUrl.split(",")[1];

            // Calculate tile dimensions (last tiles may be smaller)
            const tileWidth = Math.min(viewportWidth, totalWidth - actualScrollX);
            const tileHeight = Math.min(viewportHeight, totalHeight - actualScrollY);

            tiles.push({
              screenshot: base64,
              x: actualScrollX,
              y: actualScrollY,
              width: tileWidth,
              height: tileHeight,
              row,
              col
            });

            console.log(`[Qontinui] Captured tile ${tiles.length}/${totalTiles} at (${actualScrollX}, ${actualScrollY})`);

            // Send progress update
            sendProgress(tiles.length, totalTiles, 'capturing');
          }
        }

        // Send stitching phase progress
        sendProgress(totalTiles, totalTiles, 'stitching');

        // Restore original scroll position and show fixed elements
        await chrome.scripting.executeScript({
          target: { tabId: targetTab.id },
          func: (origX, origY, _shouldRestoreFixed) => {
            window.scrollTo({
              left: origX,
              top: origY,
              behavior: 'instant'
            });

            // Note: We can't restore the original elements since we don't have references
            // In a future version, we could use a unique identifier system
          },
          args: [originalScrollX, originalScrollY, hideFixedElements]
        });

        // Send completion progress
        sendProgress(totalTiles, totalTiles, 'complete');

        return {
          tiles,
          totalWidth,
          totalHeight,
          viewportWidth,
          viewportHeight,
          capturedAt: Date.now(),
          tabId: targetTab.id,
          url: targetTab.url
        };
      } catch (e) {
        console.error("[Qontinui] Full page screenshot capture failed:", e.message);
        throw new Error(`Full page screenshot capture failed: ${e.message}`);
      }
    }

    // =========================================================================
    // Capture Session Management - State Machine Discovery
    // =========================================================================

    case "startCaptureSession": {
      // Start a new capture session for state machine discovery
      return startCaptureSession();
    }

    case "endCaptureSession": {
      // End the current capture session and return all data
      const session = endCaptureSession();
      if (!session) {
        throw new Error("No active capture session");
      }
      return session;
    }

    case "getCaptureSessionStatus": {
      // Get current session status
      return getCaptureSessionStatus();
    }

    case "createCapture": {
      // Create a capture record from current elements
      // This is called automatically when elements are refreshed
      const { url, title, elements, triggeredBy } = params;
      return createCaptureRecord(url, title, elements, triggeredBy);
    }

    case "recordAction": {
      // Record an action that was performed
      const { actionType, targetFingerprint, beforeCaptureId, afterCaptureId } = params;
      const record = recordAction(actionType, targetFingerprint, beforeCaptureId, afterCaptureId);
      if (!record) {
        throw new Error("Failed to record action");
      }
      return record;
    }

    case "exportCaptureSession": {
      // Export the current session data without ending it
      if (!activeCaptureSession) {
        throw new Error("No active capture session");
      }
      return {
        ...activeCaptureSession,
        exportedAt: Date.now(),
        fingerprintCatalog: Object.fromEntries(activeCaptureSession.fingerprintCatalog)
      };
    }

    // =========================================================================
    // Recording Actions - Manual navigation capture
    // =========================================================================

    case "startRecording": {
      // Start recording on a specific tab
      const targetTabId = params.tabId || selectedExplorationTabId;
      if (!targetTabId) {
        // Try to get active tab
        const [activeTab] = await chrome.tabs.query({ active: true, currentWindow: true });
        if (!activeTab?.id) {
          throw new Error("No tab specified and no active tab found");
        }
        params.tabId = activeTab.id;
      }
      const tabId = params.tabId || targetTabId;

      if (activeRecordingSession) {
        throw new Error(`Recording already in progress on tab ${activeRecordingSession.tabId}`);
      }

      // Inject recorder scripts if not already there
      try {
        await chrome.scripting.executeScript({
          target: { tabId: tabId },
          files: ["content-scripts/ui-bridge-recorder.js"],
          world: "MAIN"
        });
        await chrome.scripting.executeScript({
          target: { tabId: tabId },
          files: ["content-scripts/ui-bridge-recorder-bridge.js"],
          world: "ISOLATED"
        });
      } catch (e) {
        console.log("[Qontinui] Could not inject recorder scripts:", e.message);
      }

      // Small delay for scripts to initialize
      await new Promise(r => setTimeout(r, 100));

      // Send start command to content script
      return new Promise((resolve, reject) => {
        chrome.tabs.sendMessage(
          tabId,
          { type: "RECORDER_COMMAND", action: "startRecording", params: params },
          (response) => {
            if (chrome.runtime.lastError) {
              reject(new Error(chrome.runtime.lastError.message || "Failed to start recording"));
              return;
            }
            if (response?.success && response.data?.success) {
              // Initialize recording session
              activeRecordingSession = {
                tabId: tabId,
                startTime: Date.now(),
                snapshots: [],
                options: params,
              };
              // Store initial snapshot if provided
              if (response.data.initialSnapshot) {
                activeRecordingSession.snapshots.push(response.data.initialSnapshot);
              }
              console.log("[Qontinui] Recording started on tab", tabId);
              resolve({
                success: true,
                tabId: tabId,
                message: "Recording started",
                initialSnapshot: response.data.initialSnapshot,
              });
            } else {
              reject(new Error(response?.data?.error || response?.error || "Failed to start recording"));
            }
          }
        );
      });
    }

    case "stopRecording": {
      if (!activeRecordingSession) {
        throw new Error("No recording in progress");
      }

      const tabId = activeRecordingSession.tabId;
      const session = activeRecordingSession;

      return new Promise((resolve, _reject) => {
        chrome.tabs.sendMessage(
          tabId,
          { type: "RECORDER_COMMAND", action: "stopRecording", params: {} },
          (_response) => {
            if (chrome.runtime.lastError) {
              // Tab might be closed, still return what we have
              console.warn("[Qontinui] Could not stop recording cleanly:", chrome.runtime.lastError.message);
            }

            const result = {
              success: true,
              tabId: tabId,
              duration: Date.now() - session.startTime,
              snapshotCount: session.snapshots.length,
              snapshots: session.snapshots,
            };

            // Clear recording session
            activeRecordingSession = null;
            console.log("[Qontinui] Recording stopped. Captured", result.snapshotCount, "snapshots");
            resolve(result);
          }
        );
      });
    }

    case "getRecordingStatus": {
      if (!activeRecordingSession) {
        return {
          isRecording: false,
          tabId: null,
          snapshotCount: 0,
        };
      }
      return {
        isRecording: true,
        tabId: activeRecordingSession.tabId,
        snapshotCount: activeRecordingSession.snapshots.length,
        duration: Date.now() - activeRecordingSession.startTime,
      };
    }

    case "getRecordingSnapshots": {
      if (!activeRecordingSession) {
        throw new Error("No recording in progress");
      }
      return {
        tabId: activeRecordingSession.tabId,
        snapshotCount: activeRecordingSession.snapshots.length,
        snapshots: activeRecordingSession.snapshots,
      };
    }

    case "captureNow": {
      // Manually trigger a capture during recording
      if (!activeRecordingSession) {
        throw new Error("No recording in progress");
      }

      const tabId = activeRecordingSession.tabId;
      return new Promise((resolve, reject) => {
        chrome.tabs.sendMessage(
          tabId,
          { type: "RECORDER_COMMAND", action: "captureNow", params: {} },
          (response) => {
            if (chrome.runtime.lastError) {
              reject(new Error(chrome.runtime.lastError.message));
              return;
            }
            resolve(response?.data || { success: false });
          }
        );
      });
    }
  }

  // For other actions, get the target tab
  const tab = await getTargetTab();
  if (!tab || !tab.id) {
    throw new Error("No target tab available. Select a tab or ensure a browser tab is active.");
  }

  // Handle exploration actions
  switch (action) {
    case "ping":
      return { available: true, tabId: tab.id, url: tab.url, title: tab.title, isSelectedTab: tab.id === selectedExplorationTabId };

    case "connect": {
      // Connect to the tab (verify UI Bridge is available)
      // Uses retry logic with exponential backoff to handle transient connection failures
      const maxRetries = 3;
      const baseDelay = 200;

      for (let attempt = 0; attempt < maxRetries; attempt++) {
        // Try to inject content scripts first
        try {
          await chrome.scripting.executeScript({
            target: { tabId: tab.id },
            files: ["content-scripts/ui-bridge-inspector.js"],
            world: "MAIN"
          });
          await chrome.scripting.executeScript({
            target: { tabId: tab.id },
            files: ["content-scripts/ui-bridge-bridge.js"],
            world: "ISOLATED"
          });
          if (attempt === 0) {
            console.log("[Qontinui] Injected content scripts into tab", tab.id);
          }
        } catch (_e) {
          // Scripts might already be there, or page doesn't allow injection
          if (attempt === 0) {
            console.log("[Qontinui] Could not inject scripts (may already exist):", _e.message);
          }
        }

        // Wait for scripts to initialize (longer on retries)
        const delay = baseDelay * (attempt + 1);
        await new Promise(r => setTimeout(r, delay));

        try {
          const result = await new Promise((resolve, reject) => {
            chrome.tabs.sendMessage(
              tab.id,
              { type: "UI_BRIDGE_COMMAND", action: "ping", params: {} },
              (response) => {
                if (chrome.runtime.lastError) {
                  reject(new Error(chrome.runtime.lastError.message || "Failed to communicate with content script. Try refreshing the page."));
                  return;
                }
                if (response && response.success) {
                  resolve({ tabId: tab.id, url: tab.url, title: tab.title, isSelectedTab: tab.id === selectedExplorationTabId });
                } else {
                  reject(new Error(response?.error || "UI Bridge not available on this page"));
                }
              }
            );
          });
          return result; // Success - return immediately
        } catch (error) {
          const isConnectionError = error.message.includes("Could not establish connection") ||
                                    error.message.includes("Receiving end does not exist");

          if (isConnectionError && attempt < maxRetries - 1) {
            console.log(`[Qontinui] connect attempt ${attempt + 1} failed with connection error, retrying...`);
            continue;
          }
          throw error;
        }
      }

      throw new Error("connect failed after retries");
    }

    case "getElements": {
      // Get all elements with data-ui-id from the page
      // Uses retry logic with exponential backoff to handle transient connection failures
      const maxRetries = 3;
      const baseDelay = 200; // Start with 200ms delay after injection

      for (let attempt = 0; attempt < maxRetries; attempt++) {
        // Try to inject scripts if not already there
        try {
          await chrome.scripting.executeScript({
            target: { tabId: tab.id },
            files: ["content-scripts/ui-bridge-inspector.js"],
            world: "MAIN"
          });
          await chrome.scripting.executeScript({
            target: { tabId: tab.id },
            files: ["content-scripts/ui-bridge-bridge.js"],
            world: "ISOLATED"
          });
        } catch {
          // Ignore - scripts may already exist
        }

        // Wait for scripts to initialize (longer on retries)
        const delay = baseDelay * (attempt + 1);
        await new Promise(r => setTimeout(r, delay));

        try {
          const result = await new Promise((resolve, reject) => {
            chrome.tabs.sendMessage(
              tab.id,
              { type: "UI_BRIDGE_COMMAND", action: "getElements", params: params || {} },
              (response) => {
                if (chrome.runtime.lastError) {
                  reject(new Error(chrome.runtime.lastError.message));
                  return;
                }
                if (response && response.success) {
                  resolve(response.data);
                } else {
                  reject(new Error(response?.error || "Failed to get elements"));
                }
              }
            );
          });
          return result; // Success - return immediately
        } catch (error) {
          const isConnectionError = error.message.includes("Could not establish connection") ||
                                    error.message.includes("Receiving end does not exist");

          if (isConnectionError && attempt < maxRetries - 1) {
            // Transient connection error - retry after backoff
            console.log(`[Qontinui] getElements attempt ${attempt + 1} failed with connection error, retrying...`);
            continue;
          }
          // Non-connection error or final attempt - throw
          throw error;
        }
      }

      // Should not reach here, but just in case
      throw new Error("getElements failed after retries");
    }

    case "executeAction": {
      // Execute an action on an element
      // Uses retry logic with exponential backoff to handle transient connection failures
      const maxRetries = 3;
      const baseDelay = 200;

      for (let attempt = 0; attempt < maxRetries; attempt++) {
        // Try to inject scripts if not already there
        try {
          await chrome.scripting.executeScript({
            target: { tabId: tab.id },
            files: ["content-scripts/ui-bridge-inspector.js"],
            world: "MAIN"
          });
          await chrome.scripting.executeScript({
            target: { tabId: tab.id },
            files: ["content-scripts/ui-bridge-bridge.js"],
            world: "ISOLATED"
          });
        } catch {
          // Ignore - scripts may already exist
        }

        // Wait for scripts to initialize (longer on retries)
        const delay = baseDelay * (attempt + 1);
        await new Promise(r => setTimeout(r, delay));

        try {
          const result = await new Promise((resolve, reject) => {
            chrome.tabs.sendMessage(
              tab.id,
              { type: "UI_BRIDGE_COMMAND", action: "executeAction", params },
              (response) => {
                if (chrome.runtime.lastError) {
                  reject(new Error(chrome.runtime.lastError.message));
                  return;
                }
                if (response && response.success) {
                  resolve(response.data);
                } else {
                  reject(new Error(response?.error || "Failed to execute action"));
                }
              }
            );
          });
          return result; // Success - return immediately
        } catch (error) {
          const isConnectionError = error.message.includes("Could not establish connection") ||
                                    error.message.includes("Receiving end does not exist");

          if (isConnectionError && attempt < maxRetries - 1) {
            console.log(`[Qontinui] executeAction attempt ${attempt + 1} failed with connection error, retrying...`);
            continue;
          }
          throw error;
        }
      }

      throw new Error("executeAction failed after retries");
    }

    case "captureSnapshot": {
      // Capture a DOM snapshot
      // Uses retry logic with exponential backoff to handle transient connection failures
      const maxRetries = 3;
      const baseDelay = 200;

      for (let attempt = 0; attempt < maxRetries; attempt++) {
        // Try to inject scripts if not already there
        try {
          await chrome.scripting.executeScript({
            target: { tabId: tab.id },
            files: ["content-scripts/ui-bridge-inspector.js"],
            world: "MAIN"
          });
          await chrome.scripting.executeScript({
            target: { tabId: tab.id },
            files: ["content-scripts/ui-bridge-bridge.js"],
            world: "ISOLATED"
          });
        } catch {
          // Ignore - scripts may already exist
        }

        // Wait for scripts to initialize (longer on retries)
        const delay = baseDelay * (attempt + 1);
        await new Promise(r => setTimeout(r, delay));

        try {
          const result = await new Promise((resolve, reject) => {
            chrome.tabs.sendMessage(
              tab.id,
              { type: "UI_BRIDGE_COMMAND", action: "getStateSnapshot", params: params || {} },
              (response) => {
                if (chrome.runtime.lastError) {
                  reject(new Error(chrome.runtime.lastError.message));
                  return;
                }
                if (response && response.success) {
                  // Add URL and title to the snapshot
                  const snapshot = response.data || {};
                  snapshot.url = tab.url;
                  snapshot.title = tab.title;
                  resolve(snapshot);
                } else {
                  reject(new Error(response?.error || "Failed to capture snapshot"));
                }
              }
            );
          });
          return result; // Success - return immediately
        } catch (error) {
          const isConnectionError = error.message.includes("Could not establish connection") ||
                                    error.message.includes("Receiving end does not exist");

          if (isConnectionError && attempt < maxRetries - 1) {
            console.log(`[Qontinui] captureSnapshot attempt ${attempt + 1} failed with connection error, retrying...`);
            continue;
          }
          throw error;
        }
      }

      throw new Error("captureSnapshot failed after retries");
    }

    default:
      throw new Error(`Unknown action: ${action}`);
  }
}

/**
 * Send a response back to the runner via WebSocket
 */
function sendWebSocketResponse(requestId, success, data, error = null) {
  sendWebSocketMessage({
    type: "EXPLORATION_RESPONSE",
    requestId,
    success,
    data,
    error,
  });
}

/**
 * Send a message to the runner via WebSocket
 */
function sendWebSocketMessage(message) {
  if (wsConnection && wsConnection.readyState === WebSocket.OPEN) {
    wsConnection.send(JSON.stringify(message));
  } else {
    console.warn("[Qontinui] Cannot send WebSocket message - not connected");
  }
}

/**
 * Send a request to the runner and wait for response
 */
function _sendWebSocketRequest(action, params = {}) {
  return new Promise((resolve, reject) => {
    if (!wsConnection || wsConnection.readyState !== WebSocket.OPEN) {
      reject(new Error("WebSocket not connected"));
      return;
    }

    const requestId = `ext-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
    const timeout = setTimeout(() => {
      wsPendingRequests.delete(requestId);
      reject(new Error(`Request timed out: ${action}`));
    }, 30000); // 30 second timeout

    wsPendingRequests.set(requestId, { resolve, reject, timeout });

    sendWebSocketMessage({
      type: "EXTENSION_REQUEST",
      requestId,
      action,
      params,
    });
  });
}

/**
 * Check if WebSocket is connected to runner
 */
function isWebSocketConnected() {
  return wsConnected && wsConnection && wsConnection.readyState === WebSocket.OPEN;
}

/**
 * Initialize WebSocket connection after a delay
 * This is called at the end of the file after all functions are defined
 */
function initializeWebSocket() {
  // Delay to allow service worker to fully initialize
  setTimeout(() => {
    // Check if runner is available before trying to connect
    fetch(`${RUNNER_API}/health`, { method: "GET" })
      .then((response) => {
        if (response.ok) {
          console.log("[Qontinui] Runner available, connecting WebSocket");
          connectWebSocket();
        } else {
          console.log("[Qontinui] Runner returned non-OK status, scheduling reconnect");
          scheduleReconnect();
        }
      })
      .catch(() => {
        console.log("[Qontinui] Runner not available, scheduling WebSocket reconnect");
        scheduleReconnect();
      });
  }, 1000);
}

// =============================================================================
// Request Recording State (persisted via chrome.storage.session)
// =============================================================================

let isRecording = false;
let capturedRequests = [];
let requestIdToData = new Map(); // Track request data by ID
let interceptedBodies = new Map(); // Bodies captured by content script (keyed by URL+method+timestamp)

// Restore state from storage on startup
chrome.storage.session.get(["isRecording", "capturedRequests"], (result) => {
  if (result.isRecording !== undefined) {
    isRecording = result.isRecording;
    console.log("[Qontinui] Restored recording state:", isRecording);
  }
  if (result.capturedRequests) {
    capturedRequests = result.capturedRequests;
    console.log("[Qontinui] Restored", capturedRequests.length, "captured requests");
  }
});

/**
 * Persist recording state to storage
 */
function persistState() {
  chrome.storage.session.set({
    isRecording: isRecording,
    capturedRequests: capturedRequests,
  });
}

// Patterns to filter out (noise)
const FILTER_PATTERNS = [
  /\/api\/dev-debug\//,           // Dev debug endpoints
  /\/monitors$/,                   // Monitor polling
  /\/status$/,                     // Status polling
  /\/health$/,                     // Health checks
  /\/auth\/jwt\/refresh/,          // Token refresh
  /\/auth\/refresh/,               // Token refresh
  /\/_next\//,                     // Next.js internals
  /\/favicon\.ico/,                // Favicons
  /\.(js|css|png|jpg|jpeg|gif|svg|woff|woff2|ttf|eot)(\?|$)/i, // Static assets
  /^chrome-extension:\/\//,        // Extension requests
  /sockjs-node/,                   // Hot reload
  /webpack/,                       // Webpack dev server
];

// Domains to completely ignore (streaming, analytics, ads, etc.)
const IGNORED_DOMAINS = [
  /googlevideo\.com$/,             // YouTube video streaming
  /youtube\.com$/,                 // YouTube API
  /accounts\.google\.com$/,        // Google auth
  /accounts\.youtube\.com$/,       // YouTube auth
  /google-analytics\.com$/,        // Analytics
  /googletagmanager\.com$/,        // Tag manager
  /doubleclick\.net$/,             // Ads
  /facebook\.com$/,                // Facebook
  /fbcdn\.net$/,                   // Facebook CDN
  /twitter\.com$/,                 // Twitter
  /analytics/,                     // Generic analytics
  /telemetry/,                     // Telemetry
  /sentry\.io$/,                   // Error tracking
  /hotjar\.com$/,                  // Heatmaps
  /intercom\.io$/,                 // Chat widgets
  /clarity\.ms$/,                  // Microsoft Clarity
];

// Methods to prioritize (show first)
const PRIORITY_METHODS = ["POST", "PUT", "PATCH", "DELETE"];

/**
 * Check if a URL should be filtered out
 */
function shouldFilterUrl(url) {
  // Check URL patterns
  if (FILTER_PATTERNS.some(pattern => pattern.test(url))) {
    return true;
  }

  // Check domain
  try {
    const urlObj = new URL(url);
    if (IGNORED_DOMAINS.some(pattern => pattern.test(urlObj.hostname))) {
      return true;
    }
  } catch {
    // Invalid URL, don't filter
  }

  return false;
}

/**
 * Start recording requests
 */
function startRecording() {
  isRecording = true;
  capturedRequests = [];
  requestIdToData.clear();
  interceptedBodies.clear();
  persistState();
  console.log("[Qontinui] Started recording API requests");
}

/**
 * Generate a key for matching intercepted bodies to webRequest data
 */
function getBodyMatchKey(method, url) {
  try {
    const urlObj = new URL(url);
    // Normalize URL (remove trailing slash, lowercase host)
    return `${method}:${urlObj.host.toLowerCase()}${urlObj.pathname}${urlObj.search}`;
  } catch {
    return `${method}:${url}`;
  }
}

/**
 * Find a matching intercepted body for a request
 * Returns the body if found within a time window (5 seconds)
 */
function findInterceptedBody(method, url, timestamp) {
  const key = getBodyMatchKey(method, url);

  // Look for matches within 5 seconds of the webRequest timestamp
  const timeWindow = 5000;
  let bestMatch = null;
  let bestTimeDiff = Infinity;

  for (const [entryKey, entry] of interceptedBodies.entries()) {
    if (entryKey.startsWith(key)) {
      const timeDiff = Math.abs(entry.timestamp - timestamp);
      if (timeDiff < timeWindow && timeDiff < bestTimeDiff) {
        bestMatch = entry;
        bestTimeDiff = timeDiff;
      }
    }
  }

  if (bestMatch) {
    console.log("[Qontinui] Found intercepted body for", method, url, "from", bestMatch.source);
    return bestMatch.body;
  }
  return null;
}

/**
 * Stop recording requests
 */
function stopRecording() {
  isRecording = false;
  persistState();
  console.log("[Qontinui] Stopped recording. Captured:", capturedRequests.length, "requests");
}

/**
 * Get a deduplication key for a request (method + host + path, ignoring query params)
 */
function getDedupeKey(req) {
  try {
    const url = new URL(req.url);
    return `${req.method}:${url.host}${url.pathname}`;
  } catch {
    return `${req.method}:${req.url}`;
  }
}

/**
 * Get captured requests (filtered, deduplicated, and sorted)
 */
function getCapturedRequests() {
  // Deduplicate: group by method + host + path (ignoring query params), keep the most recent one
  const seen = new Map();
  for (const req of capturedRequests) {
    const key = getDedupeKey(req);
    const existing = seen.get(key);
    if (!existing || req.timestamp > existing.timestamp) {
      // For repeated requests, track the count
      const newReq = { ...req };
      if (existing) {
        newReq.repeatCount = (existing.repeatCount || 1) + 1;
      }
      seen.set(key, newReq);
    } else if (existing) {
      existing.repeatCount = (existing.repeatCount || 1) + 1;
    }
  }

  const deduplicated = Array.from(seen.values());

  // Sort: priority methods first, then by timestamp (newest first)
  return deduplicated.sort((a, b) => {
    const aPriority = PRIORITY_METHODS.includes(a.method) ? 0 : 1;
    const bPriority = PRIORITY_METHODS.includes(b.method) ? 0 : 1;
    if (aPriority !== bPriority) return aPriority - bPriority;
    return b.timestamp - a.timestamp;
  });
}

/**
 * Convert captured request to cURL command
 */
function requestToCurl(request) {
  let curl = `curl '${request.url}'`;

  if (request.method !== "GET") {
    curl += ` \\\n  -X ${request.method}`;
  }

  if (request.headers) {
    for (const [name, value] of Object.entries(request.headers)) {
      // Skip some headers that curl handles automatically
      if (["content-length", "host", "connection"].includes(name.toLowerCase())) continue;
      curl += ` \\\n  -H '${name}: ${value}'`;
    }
  }

  if (request.body) {
    // Escape single quotes in body
    const escapedBody = request.body.replace(/'/g, "'\\''");
    curl += ` \\\n  --data-raw '${escapedBody}'`;
  }

  return curl;
}

// =============================================================================
// WebRequest Listeners
// =============================================================================

// Capture request details when request starts
chrome.webRequest.onBeforeRequest.addListener(
  (details) => {
    if (!isRecording) return;
    if (details.method === "OPTIONS") return; // Skip CORS preflight
    if (shouldFilterUrl(details.url)) return;

    const timestamp = Date.now();

    // Extract request body if available from webRequest API
    let body = null;
    let bodySource = null;

    if (details.requestBody) {
      if (details.requestBody.raw && details.requestBody.raw.length > 0) {
        // Raw bytes - try to decode as text
        try {
          const decoder = new TextDecoder("utf-8");
          const bytes = details.requestBody.raw.map(r => r.bytes).filter(Boolean);
          if (bytes.length > 0) {
            body = bytes.map(b => decoder.decode(b)).join("");
            bodySource = "webRequest.raw";
          }
        } catch (e) {
          console.log("[Qontinui] Failed to decode raw body:", e.message);
        }
      } else if (details.requestBody.formData) {
        // Form data
        body = JSON.stringify(details.requestBody.formData);
        bodySource = "webRequest.formData";
      }
    }

    // Fallback: Try to find body captured by content script interceptor
    if (!body || body === "[Binary data]") {
      const interceptedBody = findInterceptedBody(details.method, details.url, timestamp);
      if (interceptedBody) {
        body = interceptedBody;
        bodySource = "interceptor";
      }
    }


    // Store initial request data
    requestIdToData.set(details.requestId, {
      id: details.requestId,
      method: details.method,
      url: details.url,
      body: body,
      bodySource: bodySource,
      timestamp: timestamp,
      headers: {}, // Will be filled by onSendHeaders
    });
  },
  { urls: ["<all_urls>"] },
  ["requestBody"]
);

// Capture request headers
// Note: "extraHeaders" is required in MV3 to capture sensitive headers like Cookie, Authorization, etc.
chrome.webRequest.onSendHeaders.addListener(
  (details) => {
    if (!isRecording) return;

    const requestData = requestIdToData.get(details.requestId);
    if (!requestData) return;

    // Convert headers array to object
    const headers = {};
    if (details.requestHeaders) {
      for (const header of details.requestHeaders) {
        headers[header.name] = header.value;
      }
    }
    requestData.headers = headers;
  },
  { urls: ["<all_urls>"] },
  ["requestHeaders", "extraHeaders"]
);

// Finalize request when it completes
chrome.webRequest.onCompleted.addListener(
  (details) => {
    if (!isRecording) return;

    const requestData = requestIdToData.get(details.requestId);
    if (!requestData) return;

    // Second chance: Try to find intercepted body if we didn't get one earlier
    // This handles the timing case where the interceptor message arrives after onBeforeRequest
    if (!requestData.body) {
      const interceptedBody = findInterceptedBody(requestData.method, requestData.url, requestData.timestamp);
      if (interceptedBody) {
        requestData.body = interceptedBody;
        requestData.bodySource = "interceptor-delayed";
        console.log("[Qontinui] Found intercepted body (delayed) for", requestData.method, requestData.url);
      }
    }

    // Add response info
    requestData.statusCode = details.statusCode;
    requestData.completed = true;

    // Add to captured requests
    capturedRequests.push(requestData);

    // Clean up
    requestIdToData.delete(details.requestId);

    // Persist state so requests aren't lost if service worker terminates
    persistState();

    console.log("[Qontinui] Captured:", details.method, details.url, details.statusCode, requestData.body ? "(with body)" : "(no body)");
  },
  { urls: ["<all_urls>"] }
);

// Handle request errors
chrome.webRequest.onErrorOccurred.addListener(
  (details) => {
    if (!isRecording) return;
    requestIdToData.delete(details.requestId);
  },
  { urls: ["<all_urls>"] }
);

/**
 * Check if the runner is available
 */
async function checkRunnerStatus() {
  try {
    const response = await fetch(`${RUNNER_API}/health`, {
      method: "GET",
      headers: { "Content-Type": "application/json" },
    });
    return response.ok;
  } catch {
    return false;
  }
}

/**
 * Import cURL command to the runner (parse only, returns parsed data)
 */
async function importCurlToRunner(curlCommand) {
  const response = await fetch(`${RUNNER_API}/api-request/import-curl`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      curl_command: curlCommand,
    }),
  });

  if (!response.ok) {
    const errorText = await response.text();
    throw new Error(`Failed to import cURL: ${response.status} ${errorText}`);
  }

  return response.json();
}

/**
 * Import cURL command and save to the API Request Library
 */
async function importCurlToLibrary(curlCommand, name = null) {
  const body = {
    curl_command: curlCommand,
  };
  if (name) {
    body.name = name;
  }
  // Mark as imported from browser extension
  body.category = "imported";

  const response = await fetch(`${RUNNER_API}/api-request/import-to-library`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });

  if (!response.ok) {
    const errorText = await response.text();
    throw new Error(`Failed to save to library: ${response.status} ${errorText}`);
  }

  return response.json();
}

/**
 * Send captured DOM to the runner
 */
async function sendDomToRunner(data) {
  const response = await fetch(`${RUNNER_API}/dom/receive`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      url: data.url,
      pageTitle: data.pageTitle,
      html: data.html,
      selector: data.selector || null,
      taskRunId: data.taskRunId || null,
    }),
  });

  if (!response.ok) {
    const errorText = await response.text();
    throw new Error(`Failed to send DOM: ${response.status} ${errorText}`);
  }

  return response.json();
}

/**
 * Capture DOM from the active tab
 */
async function captureCurrentTab(selector = null) {
  // Get the active tab
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab || !tab.id) {
    throw new Error("No active tab found");
  }

  // Inject and execute content script to capture DOM
  const results = await chrome.scripting.executeScript({
    target: { tabId: tab.id },
    func: (sel) => {
      if (sel) {
        const el = document.querySelector(sel);
        if (!el) {
          return { error: `Element not found: ${sel}` };
        }
        return {
          html: el.outerHTML,
          url: window.location.href,
          pageTitle: document.title,
          selector: sel,
        };
      }
      return {
        html: document.documentElement.outerHTML,
        url: window.location.href,
        pageTitle: document.title,
        selector: null,
      };
    },
    args: [selector],
  });

  if (!results || !results[0]) {
    throw new Error("Failed to execute content script");
  }

  const result = results[0].result;
  if (result.error) {
    throw new Error(result.error);
  }

  return result;
}

// Handle messages from popup and content scripts
chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  // Handle intercepted request body from content script
  if (message.action === "interceptedRequestBody") {
    if (!isRecording || !message.data || shouldFilterUrl(message.data.url)) {
      // Still need to respond to prevent "message port closed" error
      return false;
    }

    const data = message.data;
    const key = getBodyMatchKey(data.method, data.url) + ":" + data.timestamp;

    // Store the intercepted body for later matching
    interceptedBodies.set(key, {
      body: data.body,
      headers: data.headers,
      timestamp: data.timestamp,
      source: data.source // 'fetch' or 'xhr'
    });

    // Clean up old entries (older than 30 seconds)
    const cutoff = Date.now() - 30000;
    for (const [k, v] of interceptedBodies.entries()) {
      if (v.timestamp < cutoff) {
        interceptedBodies.delete(k);
      }
    }

    console.log("[Qontinui] Stored intercepted body from", data.source, "for", data.method, data.url);
    return false; // No async response needed
  }

  if (message.action === "checkStatus") {
    checkRunnerStatus().then((available) => {
      sendResponse({ available, wsConnected: isWebSocketConnected() });
    });
    return true; // Keep channel open for async response
  }

  // =============================================================================
  // Runner Commands from Web Page (for cloud qontinui.io)
  // This allows the qontinui.io website to send commands to the local runner
  // through the extension, bypassing CORS restrictions.
  // =============================================================================

  if (message.type === "RUNNER_COMMAND_FROM_PAGE") {
    const { requestId: _requestId, action, params } = message;
    console.log("[Qontinui] Runner command from page:", action, params);

    (async () => {
      try {
        // Execute the command via the runner's extension API
        const result = await executeExplorationCommand(action, params || {});
        sendResponse({ success: true, data: result });
      } catch (error) {
        console.error("[Qontinui] Runner command failed:", error);
        sendResponse({ success: false, error: error.message });
      }
    })();
    return true; // Async response
  }

  // =============================================================================
  // Selected Tab Management (for popup)
  // =============================================================================

  if (message.action === "getSelectedTab") {
    console.log("[Qontinui BG] getSelectedTab handler called, current state:", { selectedExplorationTabId, selectedExplorationTabInfo });
    // Verify the tab still exists
    if (selectedExplorationTabId !== null) {
      chrome.tabs.get(selectedExplorationTabId).then((tab) => {
        selectedExplorationTabInfo = { id: tab.id, url: tab.url, title: tab.title };
        console.log("[Qontinui BG] Sending selected tab response:", { selectedExplorationTabId, selectedExplorationTabInfo });
        sendResponse({
          selectedTabId: selectedExplorationTabId,
          selectedTabInfo: selectedExplorationTabInfo
        });
      }).catch(() => {
        // Tab no longer exists
        console.log("[Qontinui BG] Selected tab no longer exists, clearing");
        selectedExplorationTabId = null;
        selectedExplorationTabInfo = null;
        persistSelectedTab();
        sendResponse({ selectedTabId: null, selectedTabInfo: null });
      });
    } else {
      console.log("[Qontinui BG] No tab selected, sending null response");
      sendResponse({ selectedTabId: null, selectedTabInfo: null });
    }
    return true; // Async response
  }

  if (message.action === "clearSelectedTab") {
    selectedExplorationTabId = null;
    selectedExplorationTabInfo = null;
    persistSelectedTab();
    sendResponse({ success: true });
    return false;
  }

  if (message.action === "selectTabFromPopup") {
    const tabId = message.tabId;
    chrome.tabs.get(tabId).then((tab) => {
      selectedExplorationTabId = tabId;
      selectedExplorationTabInfo = { id: tab.id, url: tab.url, title: tab.title };
      persistSelectedTab();
      sendResponse({ success: true, selectedTabInfo: selectedExplorationTabInfo });
    }).catch((e) => {
      sendResponse({ success: false, error: e.message });
    });
    return true; // Async response
  }

  // =============================================================================
  // WebSocket Connection Commands
  // =============================================================================

  if (message.action === "getWebSocketStatus") {
    sendResponse({
      connected: isWebSocketConnected(),
      url: RUNNER_WS_URL,
    });
    return true;
  }

  if (message.action === "reconnectWebSocket") {
    // Force reconnection
    if (wsConnection) {
      wsConnection.close();
    }
    wsConnection = null;
    wsConnected = false;
    if (wsReconnectTimeout) {
      clearTimeout(wsReconnectTimeout);
      wsReconnectTimeout = null;
    }
    connectWebSocket();
    sendResponse({ success: true });
    return true;
  }

  if (message.action === "capture") {
    (async () => {
      try {
        // Capture DOM
        const domData = await captureCurrentTab(message.selector);

        // Send to runner
        const result = await sendDomToRunner(domData);

        sendResponse({
          success: true,
          capture: result.data,
          size: domData.html.length,
        });
      } catch (error) {
        sendResponse({
          success: false,
          error: error.message,
        });
      }
    })();
    return true; // Keep channel open for async response
  }

  if (message.action === "importCurl") {
    (async () => {
      try {
        const result = await importCurlToRunner(message.curlCommand);

        sendResponse({
          success: true,
          method: result.method,
          url: result.url,
        });
      } catch (error) {
        sendResponse({
          success: false,
          error: error.message,
        });
      }
    })();
    return true; // Keep channel open for async response
  }

  // Recording controls
  if (message.action === "startRecording") {
    startRecording();
    sendResponse({ success: true, isRecording: true });
    return true;
  }

  if (message.action === "stopRecording") {
    stopRecording();
    sendResponse({ success: true, isRecording: false });
    return true;
  }

  if (message.action === "getRecordingStatus") {
    sendResponse({
      isRecording: isRecording,
      requestCount: capturedRequests.length,
    });
    return true;
  }

  if (message.action === "getCapturedRequests") {
    sendResponse({
      success: true,
      requests: getCapturedRequests(),
    });
    return true;
  }

  if (message.action === "getRequestAsCurl") {
    const request = capturedRequests.find(r => r.id === message.requestId);
    if (request) {
      sendResponse({
        success: true,
        curl: requestToCurl(request),
      });
    } else {
      sendResponse({
        success: false,
        error: "Request not found",
      });
    }
    return true;
  }

  if (message.action === "sendRequestToRunner") {
    (async () => {
      try {
        const request = capturedRequests.find(r => r.id === message.requestId);
        if (!request) {
          throw new Error("Request not found");
        }

        const curl = requestToCurl(request);
        // Save to the API Request Library
        const result = await importCurlToLibrary(curl);

        sendResponse({
          success: true,
          method: result.data?.method || request.method,
          url: result.data?.url || request.url,
          name: result.data?.name,
          message: "Saved to API Request Library",
        });
      } catch (error) {
        sendResponse({
          success: false,
          error: error.message,
        });
      }
    })();
    return true;
  }

  if (message.action === "sendAllRequestsToRunner") {
    (async () => {
      try {
        if (capturedRequests.length === 0) {
          sendResponse({ success: false, error: "No requests to save" });
          return;
        }

        let savedCount = 0;
        const errors = [];

        for (const request of capturedRequests) {
          try {
            const curl = requestToCurl(request);
            await importCurlToLibrary(curl);
            savedCount++;
          } catch (error) {
            errors.push(`${request.method} ${request.url}: ${error.message}`);
          }
        }

        if (savedCount > 0) {
          sendResponse({
            success: true,
            count: savedCount,
            errors: errors.length > 0 ? errors : undefined,
          });
        } else {
          sendResponse({
            success: false,
            error: errors.join("; "),
          });
        }
      } catch (error) {
        sendResponse({
          success: false,
          error: error.message,
        });
      }
    })();
    return true;
  }

  if (message.action === "clearCapturedRequests") {
    capturedRequests = [];
    persistState();
    sendResponse({ success: true });
    return true;
  }

  // =============================================================================
  // UI Bridge Commands (from popup to content script)
  // =============================================================================

  if (message.type === "UI_BRIDGE_POPUP_COMMAND") {
    (async () => {
      try {
        // Use selected tab if available, otherwise fall back to active tab
        let tab = null;
        if (selectedExplorationTabId !== null) {
          try {
            tab = await chrome.tabs.get(selectedExplorationTabId);
          } catch {
            // Selected tab no longer exists, clear it
            selectedExplorationTabId = null;
            selectedExplorationTabInfo = null;
            persistSelectedTab();
          }
        }

        // Fall back to active tab if no selected tab
        if (!tab) {
          const [activeTab] = await chrome.tabs.query({ active: true, currentWindow: true });
          tab = activeTab;
        }

        if (!tab || !tab.id) {
          sendResponse({ success: false, error: "No active tab found" });
          return;
        }

        // Use retry logic with content script injection to handle cases where
        // the page was loaded before the extension (content scripts not injected)
        const maxRetries = 3;
        const baseDelay = 200;

        for (let attempt = 0; attempt < maxRetries; attempt++) {
          // Try to inject content scripts first (they may already exist)
          try {
            await chrome.scripting.executeScript({
              target: { tabId: tab.id },
              files: ["content-scripts/ui-bridge-inspector.js"],
              world: "MAIN"
            });
            await chrome.scripting.executeScript({
              target: { tabId: tab.id },
              files: ["content-scripts/ui-bridge-bridge.js"],
              world: "ISOLATED"
            });
            if (attempt === 0) {
              console.log("[Qontinui] Injected UI Bridge scripts into tab", tab.id);
            }
          } catch (_e) {
            // Scripts might already be there, or page doesn't allow injection
            if (attempt === 0) {
              console.log("[Qontinui] Could not inject UI Bridge scripts (may already exist):", _e.message);
            }
          }

          // Wait for scripts to initialize (longer on retries)
          const delay = baseDelay * (attempt + 1);
          await new Promise(r => setTimeout(r, delay));

          // Try to send the command (with timeout to prevent hanging)
          try {
            const result = await new Promise((resolve, reject) => {
              const timeout = setTimeout(() => {
                reject(new Error("Content script response timeout"));
              }, 2000); // 2 second timeout per attempt

              chrome.tabs.sendMessage(
                tab.id,
                {
                  type: "UI_BRIDGE_COMMAND",
                  action: message.action,
                  params: message.params || {},
                },
                (response) => {
                  clearTimeout(timeout);
                  if (chrome.runtime.lastError) {
                    reject(new Error(chrome.runtime.lastError.message || "Failed to communicate with content script"));
                    return;
                  }
                  resolve(response || { success: false, error: "No response from content script" });
                }
              );
            });
            // Success - send response and return
            sendResponse(result);
            return;
          } catch (error) {
            const isRetryableError = error.message.includes("Could not establish connection") ||
                                     error.message.includes("Receiving end does not exist") ||
                                     error.message.includes("timeout");

            if (isRetryableError && attempt < maxRetries - 1) {
              console.log(`[Qontinui] UI Bridge command attempt ${attempt + 1} failed (${error.message}), retrying...`);
              continue;
            }
            // Last attempt or non-retryable error - throw to outer catch
            throw error;
          }
        }

        // Should not reach here, but handle it
        sendResponse({ success: false, error: "UI Bridge command failed after retries" });
      } catch (error) {
        sendResponse({ success: false, error: error.message });
      }
    })();
    return true; // Keep channel open for async response
  }

  // =============================================================================
  // UI Bridge Events (from content script to popup)
  // =============================================================================

  if (message.type === "UI_BRIDGE_EVENT") {
    // Forward the event to all extension pages (popup)
    chrome.runtime.sendMessage(message).catch(() => {
      // Popup might not be open, ignore error
    });
    return false;
  }

  if (message.type === "UI_BRIDGE_BRIDGE_READY") {
    // Content script bridge is ready
    console.log("[Qontinui] UI Bridge bridge ready on:", message.url);
    return false;
  }

  // =============================================================================
  // Recording Events (from content script to background)
  // =============================================================================

  if (message.type === "RECORDER_SNAPSHOT") {
    // Accumulate snapshot from recording
    if (activeRecordingSession && message.snapshot) {
      activeRecordingSession.snapshots.push(message.snapshot);
      console.log("[Qontinui] Recorded snapshot", activeRecordingSession.snapshots.length, ":", message.snapshot.trigger);

      // Forward to runner via WebSocket for real-time updates
      if (wsConnected) {
        sendWebSocketMessage({
          type: "RECORDING_SNAPSHOT",
          snapshot: message.snapshot,
          sessionTabId: activeRecordingSession.tabId,
          totalSnapshots: activeRecordingSession.snapshots.length,
        });
      }
    }
    return false;
  }

  if (message.type === "RECORDER_BRIDGE_READY") {
    console.log("[Qontinui] Recorder bridge ready on:", message.url);
    return false;
  }
});

// Initialize WebSocket connection to runner (delayed to allow service worker startup)
initializeWebSocket();
