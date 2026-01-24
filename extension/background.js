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
      for (const [requestId, pending] of wsPendingRequests.entries()) {
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
  } catch (e) {
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
 * Execute an exploration command by forwarding to the active tab's content script
 */
async function executeExplorationCommand(action, params) {
  // Get the active tab
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab || !tab.id) {
    throw new Error("No active tab found");
  }

  // Handle different actions
  switch (action) {
    case "ping":
      return { available: true, tabId: tab.id, url: tab.url, title: tab.title };

    case "connect":
      // Connect to the tab (verify UI Bridge is available)
      return new Promise((resolve, reject) => {
        chrome.tabs.sendMessage(
          tab.id,
          { type: "UI_BRIDGE_COMMAND", action: "ping", params: {} },
          (response) => {
            if (chrome.runtime.lastError) {
              reject(new Error(chrome.runtime.lastError.message || "Failed to communicate with content script"));
              return;
            }
            if (response && response.success) {
              resolve({ tabId: tab.id, url: tab.url, title: tab.title });
            } else {
              reject(new Error(response?.error || "UI Bridge not available on this page"));
            }
          }
        );
      });

    case "getElements":
      // Get all elements with data-ui-id from the page
      return new Promise((resolve, reject) => {
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

    case "executeAction":
      // Execute an action on an element
      return new Promise((resolve, reject) => {
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

    case "captureSnapshot":
      // Capture a DOM snapshot
      return new Promise((resolve, reject) => {
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
function sendWebSocketRequest(action, params = {}) {
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
  } catch (e) {
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
        // Get the active tab
        const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
        if (!tab || !tab.id) {
          sendResponse({ success: false, error: "No active tab found" });
          return;
        }

        // Forward the command to the content script
        chrome.tabs.sendMessage(
          tab.id,
          {
            type: "UI_BRIDGE_COMMAND",
            action: message.action,
            params: message.params || {},
          },
          (response) => {
            if (chrome.runtime.lastError) {
              sendResponse({
                success: false,
                error: chrome.runtime.lastError.message || "Failed to communicate with content script",
              });
              return;
            }
            sendResponse(response || { success: false, error: "No response from content script" });
          }
        );
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
});

// Initialize WebSocket connection to runner (delayed to allow service worker startup)
initializeWebSocket();
