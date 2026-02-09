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
// Debug Log Ring Buffer
// =============================================================================

/**
 * Ring buffer that stores the last N debug events with timestamps.
 * Queryable via TO_OFFSCREEN_GET_DEBUG_LOG message from service worker.
 */
const DEBUG_LOG_MAX = 200;
const debugLog = [];
let debugSeq = 0;

function dbg(category, message, data = null) {
  const entry = {
    seq: debugSeq++,
    ts: Date.now(),
    iso: new Date().toISOString(),
    cat: category,
    msg: message,
  };
  if (data !== null && data !== undefined) {
    entry.data = data;
  }
  debugLog.push(entry);
  if (debugLog.length > DEBUG_LOG_MAX) {
    debugLog.shift();
  }
  // Also log to console with structured prefix
  const dataStr = data !== null && data !== undefined ? ` | ${JSON.stringify(data)}` : "";
  console.log(`[Qontinui Offscreen][${category}] ${message}${dataStr}`);
}

// =============================================================================
// WebSocket Connection State
// =============================================================================

let wsConnection = null;
let wsConnected = false;
let wsReconnectTimeout = null;
let wsPendingRequests = new Map(); // requestId -> { resolve, reject, timeout }
let keepaliveInterval = null;
let lastPongAt = null;
let lastPingSentAt = null;
let pingsSent = 0;
let pongsReceived = 0;
let reconnectCount = 0;
let connectedSince = null;
let lastDisconnectAt = null;
let lastDisconnectReason = null;
let healthCheckFailures = 0;
let consecutiveHealthCheckFailures = 0;
let messagesSent = 0;
let messagesReceived = 0;

dbg("INIT", "Offscreen document loaded");

// =============================================================================
// WebSocket Connection Management
// =============================================================================

/**
 * Connect to the runner's WebSocket endpoint.
 * Only call this after verifying the runner is available via health check.
 */
/**
 * Interpret WebSocket close codes for debugging.
 */
function describeCloseCode(code) {
  const codes = {
    1000: "Normal closure",
    1001: "Going away (page navigation or server shutdown)",
    1002: "Protocol error",
    1003: "Unsupported data",
    1005: "No status code (abnormal — browser closed without close frame)",
    1006: "Abnormal closure (no close frame received — connection dropped/network issue)",
    1007: "Invalid payload data",
    1008: "Policy violation",
    1009: "Message too big",
    1010: "Missing extension",
    1011: "Internal server error",
    1012: "Service restart",
    1013: "Try again later",
    1014: "Bad gateway",
    1015: "TLS handshake failure",
  };
  return codes[code] || `Unknown code ${code}`;
}

function connectWebSocket() {
  if (
    wsConnection &&
    (wsConnection.readyState === WebSocket.CONNECTING || wsConnection.readyState === WebSocket.OPEN)
  ) {
    dbg("WS", "connectWebSocket called but already connecting/open", {
      readyState: wsConnection.readyState,
      readyStateDesc: wsConnection.readyState === WebSocket.CONNECTING ? "CONNECTING" : "OPEN",
    });
    return;
  }

  dbg("WS", "Initiating WebSocket connection", {
    url: RUNNER_WS_URL,
    reconnectCount,
    lastDisconnectAt,
    lastDisconnectReason,
    timeSinceLastDisconnect: lastDisconnectAt ? Date.now() - lastDisconnectAt : null,
  });

  // Reset per-connection counters
  messagesSent = 0;
  messagesReceived = 0;
  pingsSent = 0;
  pongsReceived = 0;

  try {
    wsConnection = new WebSocket(RUNNER_WS_URL);

    wsConnection.onopen = () => {
      wsConnected = true;
      connectedSince = Date.now();
      lastPongAt = Date.now();
      consecutiveHealthCheckFailures = 0;

      dbg("WS", "WebSocket OPEN", {
        reconnectCount,
        connectedSince,
        timeSinceLastDisconnect: lastDisconnectAt ? Date.now() - lastDisconnectAt : null,
      });

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
        dbg("WS", "Reconnected — requesting content script re-injection", {
          reconnectCount,
        });
        notifyServiceWorker({ type: "OFFSCREEN_REQUEST_REINJECT" });
      }

      reconnectCount++;
    };

    wsConnection.onclose = (event) => {
      const connectionDuration = connectedSince ? Date.now() - connectedSince : null;
      const lastPongAge = lastPongAt ? Date.now() - lastPongAt : null;

      dbg("WS", "WebSocket CLOSE", {
        code: event.code,
        codeDesc: describeCloseCode(event.code),
        reason: event.reason || "(empty)",
        wasClean: event.wasClean,
        connectionDurationMs: connectionDuration,
        connectionDurationSec: connectionDuration ? Math.round(connectionDuration / 1000) : null,
        lastPongAgeMs: lastPongAge,
        lastPongAgeSec: lastPongAge ? Math.round(lastPongAge / 1000) : null,
        pingsSent,
        pongsReceived,
        messagesSent,
        messagesReceived,
        pendingRequests: wsPendingRequests.size,
      });

      lastDisconnectAt = Date.now();
      lastDisconnectReason = `code=${event.code} (${describeCloseCode(event.code)}) reason="${event.reason || ""}" clean=${event.wasClean}`;
      handleDisconnect();
    };

    wsConnection.onerror = (event) => {
      dbg("WS", "WebSocket ERROR", {
        // Chrome doesn't expose much in error events for security reasons
        type: event.type,
        readyState: wsConnection?.readyState,
        wasConnected: wsConnected,
      });
      wsConnected = false;
    };

    wsConnection.onmessage = (event) => {
      messagesReceived++;
      try {
        const message = JSON.parse(event.data);
        handleWebSocketMessage(message);
      } catch (e) {
        dbg("WS", "Failed to parse message", {
          error: e.message,
          dataLength: event.data?.length,
          dataPreview: typeof event.data === "string" ? event.data.slice(0, 100) : "(non-string)",
        });
      }
    };
  } catch (error) {
    dbg("WS", "WebSocket constructor threw", { error: error.message });
    scheduleReconnect();
  }
}

/**
 * Handle WebSocket disconnect: clean up state, reject pending requests,
 * notify service worker, and schedule reconnection.
 */
function handleDisconnect() {
  const pendingCount = wsPendingRequests.size;
  wsConnected = false;
  wsConnection = null;
  connectedSince = null;
  stopKeepalive();

  // Reject all pending requests
  for (const [requestId, pending] of wsPendingRequests.entries()) {
    clearTimeout(pending.timeout);
    pending.reject(new Error("WebSocket disconnected"));
    dbg("WS", "Rejected pending request on disconnect", { requestId });
  }
  wsPendingRequests.clear();

  if (pendingCount > 0) {
    dbg("WS", `Rejected ${pendingCount} pending requests on disconnect`);
  }

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
    dbg("RECONNECT", "Reconnect already scheduled, skipping");
    return;
  }
  dbg("RECONNECT", "Scheduling reconnect in 3s", {
    reconnectCount,
    consecutiveHealthCheckFailures,
  });
  wsReconnectTimeout = setTimeout(() => {
    wsReconnectTimeout = null;
    const healthCheckStart = Date.now();
    fetch(`${RUNNER_API}/health`, { method: "GET" })
      .then((response) => {
        const healthCheckMs = Date.now() - healthCheckStart;
        if (response.ok) {
          consecutiveHealthCheckFailures = 0;
          dbg("RECONNECT", "Health check OK, connecting WebSocket", {
            healthCheckMs,
          });
          connectWebSocket();
        } else {
          healthCheckFailures++;
          consecutiveHealthCheckFailures++;
          dbg("RECONNECT", "Health check returned non-OK", {
            status: response.status,
            statusText: response.statusText,
            healthCheckMs,
            consecutiveFailures: consecutiveHealthCheckFailures,
          });
          scheduleReconnect();
        }
      })
      .catch((error) => {
        healthCheckFailures++;
        consecutiveHealthCheckFailures++;
        const healthCheckMs = Date.now() - healthCheckStart;
        // Only log periodically to avoid noise when runner is down for extended time
        if (consecutiveHealthCheckFailures <= 3 || consecutiveHealthCheckFailures % 12 === 0) {
          dbg("RECONNECT", "Health check failed (runner not reachable)", {
            error: error.message,
            healthCheckMs,
            consecutiveFailures: consecutiveHealthCheckFailures,
            totalFailures: healthCheckFailures,
          });
        }
        scheduleReconnect();
      });
  }, 3000);
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
  dbg("KEEPALIVE", "Starting keepalive interval (20s)");
  keepaliveInterval = setInterval(() => {
    if (wsConnection && wsConnection.readyState === WebSocket.OPEN) {
      const now = Date.now();
      lastPingSentAt = now;
      pingsSent++;

      // Send application-level ping
      sendWebSocketMessage({ type: "PING", timestamp: now });

      const pongAge = lastPongAt ? now - lastPongAt : null;
      const connectionAge = connectedSince ? now - connectedSince : null;

      dbg("KEEPALIVE", "Ping sent", {
        pingNumber: pingsSent,
        pongsReceived,
        lastPongAgeMs: pongAge,
        lastPongAgeSec: pongAge ? Math.round(pongAge / 1000) : null,
        connectionAgeSec: connectionAge ? Math.round(connectionAge / 1000) : null,
        bufferedAmount: wsConnection.bufferedAmount,
      });

      // Check for stale connection: if no pong for 45 seconds, reconnect
      if (lastPongAt && pongAge > 45000) {
        dbg("KEEPALIVE", "STALE CONNECTION DETECTED — no pong for 45s, forcing close", {
          lastPongAgeMs: pongAge,
          pingsSent,
          pongsReceived,
          missedPongs: pingsSent - pongsReceived,
          bufferedAmount: wsConnection.bufferedAmount,
        });
        wsConnection.close();
      }
    } else {
      dbg("KEEPALIVE", "Skipped ping — WebSocket not OPEN", {
        exists: !!wsConnection,
        readyState: wsConnection?.readyState,
      });
    }
  }, 20000); // Every 20 seconds
}

/**
 * Stop the keepalive interval.
 */
function stopKeepalive() {
  if (keepaliveInterval) {
    dbg("KEEPALIVE", "Stopping keepalive interval");
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
    dbg("CMD", "Received exploration command from runner", {
      action: message.action,
      requestId,
      hasParams: !!(message.params && Object.keys(message.params).length > 0),
    });
    const cmdStart = Date.now();
    try {
      const result = await sendToServiceWorker({
        type: "OFFSCREEN_EXPLORATION_COMMAND",
        action: message.action,
        params: message.params || {},
        requestId,
      });
      dbg("CMD", "Exploration command succeeded", {
        action: message.action,
        requestId,
        durationMs: Date.now() - cmdStart,
      });
      sendWebSocketResponse(requestId, true, result);
    } catch (error) {
      dbg("CMD", "Exploration command FAILED", {
        action: message.action,
        requestId,
        error: error.message,
        durationMs: Date.now() - cmdStart,
      });
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
    const now = Date.now();
    const rtt = lastPingSentAt ? now - lastPingSentAt : null;
    lastPongAt = now;
    pongsReceived++;
    dbg("KEEPALIVE", "Pong received", {
      rttMs: rtt,
      pongsReceived,
      pingsSent,
    });
  } else {
    dbg("MSG", "Unknown message type", { type, requestId });
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
    messagesSent++;
    wsConnection.send(JSON.stringify(message));
  } else {
    dbg("WS", "Cannot send message — not connected", {
      type: message.type,
      readyState: wsConnection?.readyState,
      wsExists: !!wsConnection,
    });
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
    const startTime = Date.now();
    const timeout = setTimeout(() => {
      dbg("SW", "Service worker response TIMEOUT (30s)", {
        messageType: message.type,
        action: message.action,
      });
      reject(new Error("Service worker response timeout (30s)"));
    }, 30000);

    try {
      chrome.runtime.sendMessage(message, (response) => {
        clearTimeout(timeout);
        const elapsed = Date.now() - startTime;
        if (chrome.runtime.lastError) {
          dbg("SW", "Service worker response error", {
            messageType: message.type,
            action: message.action,
            error: chrome.runtime.lastError.message,
            elapsedMs: elapsed,
          });
          reject(new Error(chrome.runtime.lastError.message));
          return;
        }
        if (response && response.success === false) {
          dbg("SW", "Service worker returned failure", {
            messageType: message.type,
            action: message.action,
            error: response.error,
            elapsedMs: elapsed,
          });
          reject(new Error(response.error || "Command failed"));
          return;
        }
        resolve(response?.data !== undefined ? response.data : response);
      });
    } catch (error) {
      clearTimeout(timeout);
      dbg("SW", "chrome.runtime.sendMessage threw", {
        messageType: message.type,
        error: error.message,
      });
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
      if (chrome.runtime.lastError) {
        dbg("SW", "Notification delivery failed (non-critical)", {
          messageType: message.type,
          error: chrome.runtime.lastError.message,
        });
      }
    });
  } catch (error) {
    dbg("SW", "notifyServiceWorker threw (non-critical)", {
      messageType: message.type,
      error: error.message,
    });
  }
}

// =============================================================================
// Incoming Messages from Service Worker
// =============================================================================

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  // IMPORTANT: Only handle messages explicitly addressed to us (TO_OFFSCREEN_* prefix).
  // chrome.runtime.sendMessage is a broadcast — popup and service worker messages
  // also arrive here. We must ignore them to avoid interfering with the response
  // channel (returning false without calling sendResponse can cause Chrome to close
  // the message port before the service worker's async handler responds).
  if (!message.type || !message.type.startsWith("TO_OFFSCREEN_")) {
    return false; // Not for us — let other listeners handle it
  }

  // Service worker wants to send data over WebSocket (e.g. recording snapshot)
  if (message.type === "TO_OFFSCREEN_WS_SEND") {
    sendWebSocketMessage(message.payload);
    sendResponse({ success: true });
    return false;
  }

  // Service worker is querying connection status
  if (message.type === "TO_OFFSCREEN_GET_STATUS") {
    const now = Date.now();
    sendResponse({
      connected: wsConnected,
      url: RUNNER_WS_URL,
      lastPongAt,
      lastPingSentAt,
      connectedSince,
      reconnectCount,
      connectionAgeSec: connectedSince ? Math.round((now - connectedSince) / 1000) : null,
      lastPongAgeSec: lastPongAt ? Math.round((now - lastPongAt) / 1000) : null,
      pingsSent,
      pongsReceived,
      messagesSent,
      messagesReceived,
      healthCheckFailures,
      consecutiveHealthCheckFailures,
      lastDisconnectAt,
      lastDisconnectReason,
      pendingRequests: wsPendingRequests.size,
      wsReadyState: wsConnection?.readyState ?? null,
      bufferedAmount: wsConnection?.bufferedAmount ?? null,
    });
    return false;
  }

  // Service worker requests a forced reconnect
  if (message.type === "TO_OFFSCREEN_RECONNECT") {
    dbg("WS", "Forced reconnect requested by service worker");
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

  // Query the debug log ring buffer
  if (message.type === "TO_OFFSCREEN_GET_DEBUG_LOG") {
    const limit = message.limit || DEBUG_LOG_MAX;
    const since = message.since || 0;
    const category = message.category || null;

    let entries = debugLog;
    if (since > 0) {
      entries = entries.filter((e) => e.ts >= since);
    }
    if (category) {
      entries = entries.filter((e) => e.cat === category);
    }
    entries = entries.slice(-limit);

    sendResponse({
      entries,
      totalEntries: debugLog.length,
      oldestSeq: debugLog.length > 0 ? debugLog[0].seq : null,
      newestSeq: debugLog.length > 0 ? debugLog[debugLog.length - 1].seq : null,
    });
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
  dbg("INIT", "Starting initial health check");
  const initStart = Date.now();
  fetch(`${RUNNER_API}/health`, { method: "GET" })
    .then((response) => {
      if (response.ok) {
        dbg("INIT", "Runner available, connecting WebSocket", {
          healthCheckMs: Date.now() - initStart,
        });
        connectWebSocket();
      } else {
        dbg("INIT", "Runner returned non-OK status", {
          status: response.status,
          healthCheckMs: Date.now() - initStart,
        });
        scheduleReconnect();
      }
    })
    .catch((error) => {
      dbg("INIT", "Runner not available", {
        error: error.message,
        healthCheckMs: Date.now() - initStart,
      });
      scheduleReconnect();
    });
}, 500);
