/**
 * Offscreen Document - WebSocket Connection Manager
 *
 * This offscreen document owns the persistent WebSocket connection to the
 * qontinui-runner. Unlike the service worker, offscreen documents are NOT
 * subject to Chrome's ~30 second inactivity termination, so the WebSocket
 * connection stays alive indefinitely.
 *
 * Architecture:
 *   Runner (Rust:9876) <--WebSocket--> Offscreen Doc <--chrome.runtime.sendMessage--> Service Worker
 *
 * The service worker handles Chrome API operations (tabs, scripting, etc.)
 * while this document handles connection management exclusively.
 */

const RUNNER_API = "http://localhost:9876";
const RUNNER_WS_URL = "ws://localhost:9876/ws/extension";

// =============================================================================
// WebSocket Connection State
// =============================================================================

let wsConnection = null;
let wsConnected = false;
let wsReconnectTimeout = null;
let wsPendingRequests = new Map(); // requestId -> { resolve, reject, timeout }
let keepaliveInterval = null;
let lastPongAt = null;
let reconnectCount = 0;
let connectedSince = null;

// =============================================================================
// WebSocket Connection Management
// =============================================================================

/**
 * Connect to the runner's WebSocket endpoint.
 * Only call this after verifying the runner is available via health check.
 */
function connectWebSocket() {
  if (
    wsConnection &&
    (wsConnection.readyState === WebSocket.CONNECTING ||
      wsConnection.readyState === WebSocket.OPEN)
  ) {
    return;
  }

  console.log("[Qontinui Offscreen] Connecting to runner WebSocket:", RUNNER_WS_URL);

  try {
    wsConnection = new WebSocket(RUNNER_WS_URL);

    wsConnection.onopen = () => {
      console.log("[Qontinui Offscreen] WebSocket connected to runner");
      wsConnected = true;
      connectedSince = Date.now();
      lastPongAt = Date.now();

      // Clear any pending reconnect
      if (wsReconnectTimeout) {
        clearTimeout(wsReconnectTimeout);
        wsReconnectTimeout = null;
      }

      // Start keepalive ping interval
      startKeepalive();

      // Notify service worker that we're connected
      notifyServiceWorker({
        type: "OFFSCREEN_WS_STATE",
        connected: true,
        reconnectCount,
      });

      // On reconnect (not first connection), ask service worker to re-inject
      // content scripts into the selected tab
      if (reconnectCount > 0) {
        console.log(
          "[Qontinui Offscreen] Reconnected (attempt",
          reconnectCount,
          ") — requesting content script re-injection",
        );
        notifyServiceWorker({ type: "OFFSCREEN_REQUEST_REINJECT" });
      }

      reconnectCount++;
    };

    wsConnection.onclose = (event) => {
      console.log(
        "[Qontinui Offscreen] WebSocket disconnected:",
        event.code,
        event.reason,
      );
      handleDisconnect();
    };

    wsConnection.onerror = () => {
      // Chrome logs WebSocket errors automatically.
      // This is expected when the runner is not available.
      wsConnected = false;
    };

    wsConnection.onmessage = (event) => {
      try {
        const message = JSON.parse(event.data);
        handleWebSocketMessage(message);
      } catch (e) {
        console.error("[Qontinui Offscreen] Failed to parse WebSocket message:", e);
      }
    };
  } catch {
    scheduleReconnect();
  }
}

/**
 * Handle WebSocket disconnect: clean up state, reject pending requests,
 * notify service worker, and schedule reconnection.
 */
function handleDisconnect() {
  wsConnected = false;
  wsConnection = null;
  connectedSince = null;
  stopKeepalive();

  // Reject all pending requests
  for (const [_requestId, pending] of wsPendingRequests.entries()) {
    clearTimeout(pending.timeout);
    pending.reject(new Error("WebSocket disconnected"));
  }
  wsPendingRequests.clear();

  // Notify service worker
  notifyServiceWorker({
    type: "OFFSCREEN_WS_STATE",
    connected: false,
    reconnectCount,
  });

  // Schedule reconnection
  scheduleReconnect();
}

/**
 * Schedule WebSocket reconnection.
 * Checks if runner is available before attempting to connect.
 */
function scheduleReconnect() {
  if (wsReconnectTimeout) {
    return; // Already scheduled
  }
  wsReconnectTimeout = setTimeout(() => {
    wsReconnectTimeout = null;
    fetch(`${RUNNER_API}/health`, { method: "GET" })
      .then((response) => {
        if (response.ok) {
          connectWebSocket();
        } else {
          scheduleReconnect();
        }
      })
      .catch(() => {
        scheduleReconnect();
      });
  }, 5000);
}

// =============================================================================
// Keepalive Ping (prevents stale connections)
// =============================================================================

/**
 * Start sending periodic ping messages over the WebSocket.
 * This serves two purposes:
 *   1. Detects dead connections (if no pong received)
 *   2. Keeps the connection active at the TCP level
 */
function startKeepalive() {
  stopKeepalive();
  keepaliveInterval = setInterval(() => {
    if (wsConnection && wsConnection.readyState === WebSocket.OPEN) {
      // Send application-level ping
      sendWebSocketMessage({ type: "PING", timestamp: Date.now() });

      // Check for stale connection: if no pong for 45 seconds, reconnect
      if (lastPongAt && Date.now() - lastPongAt > 45000) {
        console.warn(
          "[Qontinui Offscreen] No pong received for 45s, forcing reconnect",
        );
        wsConnection.close();
      }
    }
  }, 20000); // Every 20 seconds
}

/**
 * Stop the keepalive interval.
 */
function stopKeepalive() {
  if (keepaliveInterval) {
    clearInterval(keepaliveInterval);
    keepaliveInterval = null;
  }
}

// =============================================================================
// WebSocket Message Handling
// =============================================================================

/**
 * Handle incoming WebSocket messages from the runner.
 */
async function handleWebSocketMessage(message) {
  const { type, requestId } = message;

  if (type === "EXPLORATION_COMMAND") {
    // Runner is sending an exploration command to execute via the extension.
    // Forward to service worker which has access to chrome.tabs/scripting APIs.
    console.log(
      "[Qontinui Offscreen] Received exploration command:",
      message.action,
      requestId,
    );
    try {
      const result = await sendToServiceWorker({
        type: "OFFSCREEN_EXPLORATION_COMMAND",
        action: message.action,
        params: message.params || {},
        requestId,
      });
      sendWebSocketResponse(requestId, true, result);
    } catch (error) {
      console.error("[Qontinui Offscreen] Exploration command failed:", error);
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
  } else if (type === "PONG") {
    lastPongAt = Date.now();
  }
}

// =============================================================================
// WebSocket Send Helpers
// =============================================================================

/**
 * Send a message to the runner via WebSocket.
 */
function sendWebSocketMessage(message) {
  if (wsConnection && wsConnection.readyState === WebSocket.OPEN) {
    wsConnection.send(JSON.stringify(message));
  } else {
    console.warn("[Qontinui Offscreen] Cannot send WebSocket message — not connected");
  }
}

/**
 * Send a response back to the runner via WebSocket.
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

// =============================================================================
// Communication with Service Worker
// =============================================================================

/**
 * Send a message to the service worker and wait for a response.
 * Uses chrome.runtime.sendMessage which wakes the service worker if sleeping.
 */
function sendToServiceWorker(message) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      reject(new Error("Service worker response timeout (30s)"));
    }, 30000);

    try {
      chrome.runtime.sendMessage(message, (response) => {
        clearTimeout(timeout);
        if (chrome.runtime.lastError) {
          reject(new Error(chrome.runtime.lastError.message));
          return;
        }
        if (response && response.success === false) {
          reject(new Error(response.error || "Command failed"));
          return;
        }
        resolve(response?.data !== undefined ? response.data : response);
      });
    } catch (error) {
      clearTimeout(timeout);
      reject(error);
    }
  });
}

/**
 * Send a notification to the service worker (fire-and-forget).
 */
function notifyServiceWorker(message) {
  try {
    chrome.runtime.sendMessage(message, () => {
      // Ignore errors — service worker may be asleep and that's fine
      if (chrome.runtime.lastError) {
        // Swallow
      }
    });
  } catch {
    // Service worker not available, ignore
  }
}

// =============================================================================
// Incoming Messages from Service Worker
// =============================================================================

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  // Service worker wants to send data over WebSocket (e.g. recording snapshot)
  if (message.type === "TO_OFFSCREEN_WS_SEND") {
    sendWebSocketMessage(message.payload);
    sendResponse({ success: true });
    return false;
  }

  // Service worker is querying connection status
  if (message.type === "TO_OFFSCREEN_GET_STATUS") {
    sendResponse({
      connected: wsConnected,
      url: RUNNER_WS_URL,
      lastPongAt,
      connectedSince,
      reconnectCount,
      connectionAgeSec: connectedSince
        ? Math.round((Date.now() - connectedSince) / 1000)
        : null,
    });
    return false;
  }

  // Service worker requests a forced reconnect
  if (message.type === "TO_OFFSCREEN_RECONNECT") {
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
    return false;
  }

  return false;
});

// =============================================================================
// Initialization
// =============================================================================

/**
 * Initialize the WebSocket connection after a short delay
 * to let the offscreen document fully load.
 */
setTimeout(() => {
  fetch(`${RUNNER_API}/health`, { method: "GET" })
    .then((response) => {
      if (response.ok) {
        console.log("[Qontinui Offscreen] Runner available, connecting WebSocket");
        connectWebSocket();
      } else {
        console.log(
          "[Qontinui Offscreen] Runner returned non-OK status, scheduling reconnect",
        );
        scheduleReconnect();
      }
    })
    .catch(() => {
      console.log(
        "[Qontinui Offscreen] Runner not available, scheduling reconnect",
      );
      scheduleReconnect();
    });
}, 500);
