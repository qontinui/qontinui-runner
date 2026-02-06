/**
 * Popup Script for Qontinui Capture Extension
 *
 * Handles UI interactions and communicates with the background service worker.
 */

/**
 * Safe wrapper for chrome.runtime.sendMessage that handles errors gracefully.
 * Returns null if there's an error instead of throwing.
 */
function safeSendMessage(message, timeoutMs = 5000) {
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      console.debug("[Qontinui] sendMessage timeout after", timeoutMs, "ms");
      resolve(null);
    }, timeoutMs);

    try {
      chrome.runtime.sendMessage(message, (response) => {
        clearTimeout(timer);
        if (chrome.runtime.lastError) {
          // Service worker may be inactive or reloading
          console.debug("[Qontinui] sendMessage error:", chrome.runtime.lastError.message);
          resolve(null);
          return;
        }
        resolve(response);
      });
    } catch (error) {
      clearTimeout(timer);
      console.debug("[Qontinui] sendMessage exception:", error);
      resolve(null);
    }
  });
}

// DOM elements
const statusEl = document.getElementById("status");
const statusTextEl = document.getElementById("statusText");
const captureFullBtn = document.getElementById("captureFullBtn");
const captureSelectorBtn = document.getElementById("captureSelectorBtn");
const selectorInput = document.getElementById("selectorInput");
const curlInput = document.getElementById("curlInput");
const importCurlBtn = document.getElementById("importCurlBtn");
const resultEl = document.getElementById("result");

// Recorder DOM elements
const recordBtn = document.getElementById("recordBtn");
const recordBtnText = document.getElementById("recordBtnText");
const saveAllBtn = document.getElementById("saveAllBtn");
const clearRecordingsBtn = document.getElementById("clearRecordingsBtn");
const recordingIndicator = document.getElementById("recordingIndicator");
const requestCountEl = document.getElementById("requestCount");
const requestList = document.getElementById("requestList");

// Main tab elements
const mainTabs = document.querySelectorAll(".main-tab");
const domTabContent = document.getElementById("domTabContent");
const apiTabContent = document.getElementById("apiTabContent");
const uibridgeTabContent = document.getElementById("uibridgeTabContent");
const specsTabContent = document.getElementById("specsTabContent");

// UI Bridge DOM elements
const uibridgeStatusBox = document.getElementById("uibridgeStatusBox");
const uibridgeStatusText = document.getElementById("uibridgeStatusText");
const togglePickerBtn = document.getElementById("togglePickerBtn");
const togglePickerBtnText = document.getElementById("togglePickerBtnText");
const refreshElementsBtn = document.getElementById("refreshElementsBtn");
const showOverlaysBtn = document.getElementById("showOverlaysBtn");
const elementList = document.getElementById("elementList");
const activeStatesCount = document.getElementById("activeStatesCount");
const transitionsCount = document.getElementById("transitionsCount");
const viewSnapshotBtn = document.getElementById("viewSnapshotBtn");

// Specs DOM elements
const refreshSpecsBtn = document.getElementById("refreshSpecsBtn");
const specsCount = document.getElementById("specsCount");
const specsList = document.getElementById("specsList");
const specDetail = document.getElementById("specDetail");
const specDetailContent = document.getElementById("specDetailContent");
const closeSpecDetailBtn = document.getElementById("closeSpecDetailBtn");

// Sub-tab elements (for API section)
const subTabs = document.querySelectorAll(".sub-tab");
const recorderSubTab = document.getElementById("recorderSubTab");
const manualSubTab = document.getElementById("manualSubTab");

// State
let isRunnerAvailable = false;
let isCapturing = false;
let isRecording = false;
let refreshInterval = null;

// UI Bridge State
let isUIBridgeAvailable = false;
let isPickerActive = false;
let showingOverlays = false;
let uibridgeElements = [];

/**
 * Update UI based on runner availability
 */
function updateStatus(available) {
  isRunnerAvailable = available;

  if (available) {
    statusEl.className = "status connected";
    statusTextEl.textContent = "Runner connected";
    captureFullBtn.disabled = false;
    captureSelectorBtn.disabled = false;
    importCurlBtn.disabled = false;
    recordBtn.disabled = false;
    clearRecordingsBtn.disabled = false;
  } else {
    statusEl.className = "status disconnected";
    statusTextEl.textContent = "Runner not available";
    captureFullBtn.disabled = true;
    captureSelectorBtn.disabled = true;
    importCurlBtn.disabled = true;
    recordBtn.disabled = true;
    clearRecordingsBtn.disabled = true;
  }
}

/**
 * Show result message
 */
function showResult(success, message, details = "") {
  resultEl.style.display = "block";
  resultEl.className = `result ${success ? "success" : "error"}`;
  resultEl.innerHTML = `
    <div class="result-title">${success ? "Success" : "Error"}</div>
    <div>${message}</div>
    ${details ? `<div style="margin-top: 4px; opacity: 0.8;">${details}</div>` : ""}
  `;

  // Auto-hide success messages after 5 seconds
  if (success) {
    setTimeout(() => {
      resultEl.style.display = "none";
    }, 5000);
  }
}

/**
 * Format bytes to human readable size
 */
function formatSize(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * Escape HTML special characters
 */
function escapeHtml(str) {
  if (!str) return "";
  return str
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#039;");
}

/**
 * Set button loading state
 */
function setButtonLoading(button, loading, originalHtml) {
  if (loading) {
    button.disabled = true;
    button.innerHTML = '<div class="spinner"></div> Capturing...';
  } else {
    button.disabled = !isRunnerAvailable;
    button.innerHTML = originalHtml;
  }
}

/**
 * Capture DOM with optional selector
 */
async function capture(selector = null) {
  if (isCapturing || !isRunnerAvailable) return;

  isCapturing = true;
  const button = selector ? captureSelectorBtn : captureFullBtn;
  const originalHtml = button.innerHTML;

  setButtonLoading(button, true);
  resultEl.style.display = "none";

  try {
    const response = await safeSendMessage({
      action: "capture",
      selector: selector,
    });

    if (response?.success) {
      showResult(
        true,
        "DOM captured successfully!",
        `Size: ${formatSize(response.size)}${selector ? ` | Selector: ${selector}` : " | Full page"}`,
      );
    } else {
      showResult(false, response?.error || "Unknown error");
    }
  } catch (error) {
    showResult(false, error.message || "Failed to capture DOM");
  } finally {
    isCapturing = false;
    setButtonLoading(button, false, originalHtml);
  }
}

// Event listeners
captureFullBtn.addEventListener("click", () => capture(null));

captureSelectorBtn.addEventListener("click", () => {
  const selector = selectorInput.value.trim();
  if (!selector) {
    showResult(false, "Please enter a CSS selector");
    return;
  }
  capture(selector);
});

selectorInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    captureSelectorBtn.click();
  }
});

/**
 * Import cURL command
 */
async function importCurl() {
  if (isCapturing || !isRunnerAvailable) return;

  const curlCommand = curlInput.value.trim();
  if (!curlCommand) {
    showResult(false, "Please paste a cURL command");
    return;
  }

  if (!curlCommand.toLowerCase().startsWith("curl")) {
    showResult(false, "Invalid cURL command", "Must start with 'curl'");
    return;
  }

  isCapturing = true;
  const originalHtml = importCurlBtn.innerHTML;
  setButtonLoading(importCurlBtn, true);
  resultEl.style.display = "none";

  try {
    const response = await safeSendMessage({
      action: "importCurl",
      curlCommand: curlCommand,
    });

    if (response?.success) {
      showResult(true, "API request imported!", `${response.method} ${response.url}`);
      curlInput.value = ""; // Clear input on success
    } else {
      showResult(false, response?.error || "Failed to import cURL");
    }
  } catch (error) {
    showResult(false, error.message || "Failed to import cURL");
  } finally {
    isCapturing = false;
    setButtonLoading(importCurlBtn, false, originalHtml);
  }
}

importCurlBtn.addEventListener("click", importCurl);

// Check runner status on popup open
safeSendMessage({ action: "checkStatus" }).then((response) => {
  updateStatus(response?.available || false);
});

// Periodically check runner status while popup is open
setInterval(async () => {
  const response = await safeSendMessage({ action: "checkStatus" });
  updateStatus(response?.available || false);
}, 5000);

// =============================================================================
// Main Tab Switching (DOM / API)
// =============================================================================

/**
 * Switch to a specific main tab
 */
function switchToMainTab(tabId) {
  // Update tab buttons
  mainTabs.forEach((t) => {
    if (t.dataset.mainTab === tabId) {
      t.classList.add("active");
    } else {
      t.classList.remove("active");
    }
  });

  // Show/hide content
  domTabContent.classList.remove("active");
  apiTabContent.classList.remove("active");
  uibridgeTabContent.classList.remove("active");
  specsTabContent.classList.remove("active");

  if (tabId === "dom") {
    domTabContent.classList.add("active");
  } else if (tabId === "api") {
    apiTabContent.classList.add("active");
  } else if (tabId === "uibridge") {
    uibridgeTabContent.classList.add("active");
    // Check UI Bridge status when tab is opened
    checkUIBridgeStatus();
  } else if (tabId === "specs") {
    specsTabContent.classList.add("active");
    refreshSpecs();
  }

  // Persist the selected tab
  chrome.storage.local.set({ selectedPopupTab: tabId });
}

mainTabs.forEach((tab) => {
  tab.addEventListener("click", () => {
    const tabId = tab.dataset.mainTab;
    switchToMainTab(tabId);
  });
});

// Restore the previously selected popup tab
chrome.storage.local.get(["selectedPopupTab"], (result) => {
  if (result.selectedPopupTab) {
    switchToMainTab(result.selectedPopupTab);
  }
});

// =============================================================================
// Sub-Tab Switching (Record / Manual within API tab)
// =============================================================================

subTabs.forEach((tab) => {
  tab.addEventListener("click", () => {
    // Update tab buttons
    subTabs.forEach((t) => t.classList.remove("active"));
    tab.classList.add("active");

    // Show/hide content
    const tabId = tab.dataset.subTab;
    if (tabId === "recorder") {
      recorderSubTab.classList.add("active");
      manualSubTab.classList.remove("active");
    } else {
      recorderSubTab.classList.remove("active");
      manualSubTab.classList.add("active");
    }
  });
});

// =============================================================================
// Request Recorder Functions
// =============================================================================

/**
 * Update recording UI state
 */
function updateRecordingUI(recording) {
  isRecording = recording;

  if (recording) {
    recordBtn.classList.add("recording");
    recordBtnText.textContent = "Stop Recording";
    recordingIndicator.style.display = "flex";

    // Start refreshing the request list
    refreshInterval = setInterval(refreshRequestList, 1000);
  } else {
    recordBtn.classList.remove("recording");
    recordBtnText.textContent = "Start Recording";
    recordingIndicator.style.display = "none";

    // Stop refreshing
    if (refreshInterval) {
      clearInterval(refreshInterval);
      refreshInterval = null;
    }

    // Do one final refresh
    refreshRequestList();
  }
}

/**
 * Toggle recording state
 */
async function toggleRecording() {
  if (isRecording) {
    // Stop recording
    await safeSendMessage({ action: "stopRecording" });
    updateRecordingUI(false);
  } else {
    // Start recording
    await safeSendMessage({ action: "startRecording" });
    updateRecordingUI(true);
    requestList.innerHTML =
      '<div class="request-list-empty">Recording... perform actions in your app</div>';
  }
}

/**
 * Refresh the request list display
 */
async function refreshRequestList() {
  // Get recording status
  const statusResponse = await safeSendMessage({ action: "getRecordingStatus" });
  requestCountEl.textContent = statusResponse?.requestCount || 0;

  // Get captured requests
  const response = await safeSendMessage({ action: "getCapturedRequests" });

  if (!response?.success || !response?.requests || response.requests.length === 0) {
    if (!isRecording) {
      requestList.innerHTML =
        '<div class="request-list-empty">No requests captured. Click "Start Recording" to begin.</div>';
    }
    saveAllBtn.style.display = "none";
    return;
  }

  // Show Save All button when there are requests
  saveAllBtn.style.display = "flex";
  saveAllBtn.disabled = !isRunnerAvailable;

  // Build request list HTML
  const html = response.requests
    .map((req) => {
      const methodClass = req.method.toLowerCase();
      const statusClass = req.statusCode >= 200 && req.statusCode < 400 ? "success" : "error";

      // Extract host and path from URL for display
      let displayHost = "";
      let displayPath = "";
      try {
        const url = new URL(req.url);
        // Show port for localhost, or hostname for external
        if (url.hostname === "localhost" || url.hostname === "127.0.0.1") {
          displayHost = `:${url.port}`;
        } else {
          displayHost = url.hostname;
        }
        displayPath = url.pathname + url.search;
      } catch {
        displayPath = req.url;
      }

      // Body preview for POST/PUT/PATCH
      let bodyPreview = "";
      if (req.body && ["POST", "PUT", "PATCH"].includes(req.method)) {
        let preview = req.body;
        // Try to extract a meaningful preview from JSON
        try {
          const json = JSON.parse(req.body);
          // Show first few keys or a summary
          const keys = Object.keys(json).slice(0, 3);
          preview = keys.length > 0 ? `{${keys.join(", ")}...}` : req.body;
        } catch {
          preview = req.body.substring(0, 50) + (req.body.length > 50 ? "..." : "");
        }
        bodyPreview = `<div class="request-body-preview" title="${escapeHtml(req.body)}">${escapeHtml(preview)}</div>`;
      }

      // Repeat count badge
      const repeatBadge =
        req.repeatCount > 1
          ? `<span class="repeat-badge" title="Called ${req.repeatCount} times">×${req.repeatCount}</span>`
          : "";

      return `
        <div class="request-item" data-request-id="${req.id}">
          <div class="request-main-row">
            <span class="request-method ${methodClass}">${req.method}</span>
            <span class="request-host">${displayHost}</span>
            <span class="request-url" title="${req.url}">${displayPath}</span>
            ${repeatBadge}
            <span class="request-status ${statusClass}">${req.statusCode}</span>
          </div>
          ${bodyPreview}
          <div class="request-actions">
            <button class="btn-send" data-request-id="${req.id}" title="Save to API Request Library">Save</button>
            <button class="btn-copy" data-request-id="${req.id}" title="Copy as cURL">Copy</button>
          </div>
        </div>
      `;
    })
    .join("");

  requestList.innerHTML = html;

  // Add event listeners to buttons
  requestList.querySelectorAll(".btn-send").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      sendRequestToRunner(btn.dataset.requestId);
    });
  });

  requestList.querySelectorAll(".btn-copy").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      copyRequestAsCurl(btn.dataset.requestId);
    });
  });
}

/**
 * Save a captured request to the API Request Library
 */
async function sendRequestToRunner(requestId) {
  const response = await safeSendMessage({
    action: "sendRequestToRunner",
    requestId: requestId,
  });

  if (response?.success) {
    const name = response.name ? `"${response.name}"` : `${response.method} ${response.url}`;
    showResult(true, "Saved to Library!", `${name} - Open API Requests in the runner to manage`);
  } else {
    showResult(false, response?.error || "Failed to save to library");
  }
}

/**
 * Copy a captured request as cURL
 */
async function copyRequestAsCurl(requestId) {
  const response = await safeSendMessage({
    action: "getRequestAsCurl",
    requestId: requestId,
  });

  if (response?.success) {
    try {
      await navigator.clipboard.writeText(response.curl);
      showResult(true, "cURL copied to clipboard!");
    } catch {
      showResult(false, "Failed to copy to clipboard");
    }
  } else {
    showResult(false, response?.error || "Failed to copy cURL");
  }
}

/**
 * Save all captured requests to the API Request Library
 */
async function saveAllRequestsToRunner() {
  saveAllBtn.disabled = true;
  const originalHtml = saveAllBtn.innerHTML;
  saveAllBtn.innerHTML = '<div class="spinner"></div>';

  const response = await safeSendMessage({
    action: "sendAllRequestsToRunner",
  });

  saveAllBtn.innerHTML = originalHtml;
  saveAllBtn.disabled = !isRunnerAvailable;

  if (response?.success) {
    showResult(
      true,
      `Saved ${response.count} request${response.count !== 1 ? "s" : ""} to Library!`,
    );
  } else {
    showResult(false, response?.error || "Failed to save requests");
  }
}

/**
 * Clear all captured requests
 */
async function clearCapturedRequests() {
  await safeSendMessage({ action: "clearCapturedRequests" });
  requestList.innerHTML =
    '<div class="request-list-empty">No requests captured. Click "Start Recording" to begin.</div>';
  requestCountEl.textContent = "0";
  saveAllBtn.style.display = "none";
}

// Event listeners for recorder
recordBtn.addEventListener("click", toggleRecording);
saveAllBtn.addEventListener("click", saveAllRequestsToRunner);
clearRecordingsBtn.addEventListener("click", clearCapturedRequests);

// Check if recording is already in progress when popup opens
safeSendMessage({ action: "getRecordingStatus" }).then((response) => {
  if (response?.isRecording) {
    updateRecordingUI(true);
    refreshRequestList();
  }
});

// =============================================================================
// UI Bridge Functions
// =============================================================================

/**
 * Send command to UI Bridge content script via background
 */
async function sendUIBridgeCommand(action, params = {}, timeoutMs) {
  return safeSendMessage({
    type: "UI_BRIDGE_POPUP_COMMAND",
    action,
    params,
  }, timeoutMs);
}

/**
 * Update UI Bridge status display
 */
function updateUIBridgeStatus(available, message = null) {
  isUIBridgeAvailable = available;

  if (available) {
    uibridgeStatusBox.className = "status connected";
    uibridgeStatusText.textContent = message || "UI Bridge detected";
    togglePickerBtn.disabled = false;
    refreshElementsBtn.disabled = false;
    showOverlaysBtn.disabled = false;
    viewSnapshotBtn.disabled = false;
  } else {
    uibridgeStatusBox.className = "status disconnected";
    uibridgeStatusText.textContent = message || "UI Bridge not detected";
    togglePickerBtn.disabled = true;
    refreshElementsBtn.disabled = true;
    showOverlaysBtn.disabled = true;
    viewSnapshotBtn.disabled = true;
  }
}

/**
 * Check if UI Bridge is available on the current page
 */
async function checkUIBridgeStatus() {
  updateUIBridgeStatus(false, "Checking...");

  // The background handler has its own 3-attempt retry cycle that takes up to ~9s.
  // Use a generous timeout so we don't cut it short, with one extra retry as safety.
  const maxRetries = 2;
  let lastResponse = null;

  for (let attempt = 0; attempt < maxRetries; attempt++) {
    if (attempt > 0) {
      updateUIBridgeStatus(false, "Checking... (retrying)");
      await new Promise((r) => setTimeout(r, 1000));
    }

    const response = await sendUIBridgeCommand("ping", {}, 12000);

    lastResponse = response;
    console.log(`[Qontinui Popup] Ping attempt ${attempt + 1}/${maxRetries}:`, JSON.stringify(response));

    if (response?.success && response?.data?.available) {
      const version = response.data.version || "unknown";
      updateUIBridgeStatus(true, `UI Bridge v${version}`);
      refreshUIBridgeElements();
      return;
    }

    // If we got an actual response (not timeout), no point retrying
    if (response !== null) {
      break;
    }
  }

  // Show diagnostic info: what did the last response look like?
  const diag = lastResponse
    ? (lastResponse.error || `success=${lastResponse.success}, available=${lastResponse.data?.available}`)
    : "no response (timeout)";
  console.warn("[Qontinui Popup] UI Bridge not found. Last response:", lastResponse);
  updateUIBridgeStatus(false, `UI Bridge not found (${diag})`);
}

/**
 * Refresh the list of registered UI Bridge elements
 */
async function refreshUIBridgeElements() {
  if (!isUIBridgeAvailable) return;

  const response = await sendUIBridgeCommand("getElements");

  if (response?.success && response?.data) {
    uibridgeElements = response.data.elements || [];
    renderElementList();

    // Update state info
    activeStatesCount.textContent = response.data.activeStatesCount || 0;
    transitionsCount.textContent = response.data.transitionsCount || 0;
  } else {
    elementList.innerHTML = '<div class="request-list-empty">Failed to fetch elements</div>';
  }
}

/**
 * Render the element list
 */
function renderElementList() {
  if (uibridgeElements.length === 0) {
    elementList.innerHTML =
      '<div class="request-list-empty">No UI Bridge elements registered</div>';
    return;
  }

  const html = uibridgeElements
    .map((el) => {
      const typeClass = el.type === "button" ? "post" : el.type === "input" ? "put" : "get";
      const stateInfo = [];
      if (el.state?.visible === false) stateInfo.push("hidden");
      if (el.state?.enabled === false) stateInfo.push("disabled");
      if (el.state?.focused) stateInfo.push("focused");

      return `
        <div class="request-item" data-element-id="${escapeHtml(el.id)}">
          <div class="request-main-row">
            <span class="request-method ${typeClass}">${el.type || "elem"}</span>
            <span class="request-url" title="${escapeHtml(el.id)}">${escapeHtml(el.id)}</span>
            ${el.label ? `<span class="request-host">${escapeHtml(el.label)}</span>` : ""}
          </div>
          ${stateInfo.length > 0 ? `<div class="request-body-preview">${stateInfo.join(", ")}</div>` : ""}
          <div class="request-actions">
            <button class="btn-send uibridge-highlight" data-element-id="${escapeHtml(el.id)}" title="Highlight element">
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <circle cx="12" cy="12" r="10"></circle>
                <circle cx="12" cy="12" r="3"></circle>
              </svg>
            </button>
            ${el.actions?.includes("click") ? `<button class="btn-copy uibridge-action" data-element-id="${escapeHtml(el.id)}" data-action="click" title="Click">Click</button>` : ""}
          </div>
        </div>
      `;
    })
    .join("");

  elementList.innerHTML = html;

  // Add event listeners
  elementList.querySelectorAll(".uibridge-highlight").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      highlightElement(btn.dataset.elementId);
    });
  });

  elementList.querySelectorAll(".uibridge-action").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      executeUIBridgeAction(btn.dataset.elementId, btn.dataset.action);
    });
  });
}

/**
 * Toggle element picker mode
 */
async function togglePicker() {
  if (isPickerActive) {
    const response = await sendUIBridgeCommand("disablePicker");
    if (response?.success) {
      isPickerActive = false;
      togglePickerBtn.classList.remove("recording");
      togglePickerBtnText.textContent = "Start Picker";
    }
  } else {
    const response = await sendUIBridgeCommand("enablePicker");
    if (response?.success) {
      isPickerActive = true;
      togglePickerBtn.classList.add("recording");
      togglePickerBtnText.textContent = "Stop Picker";
    }
  }
}

/**
 * Toggle element overlays
 */
async function toggleOverlays() {
  if (showingOverlays) {
    const response = await sendUIBridgeCommand("hideOverlays");
    if (response?.success) {
      showingOverlays = false;
      showOverlaysBtn.innerHTML = `
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <rect x="3" y="3" width="18" height="18" rx="2" ry="2"></rect>
          <rect x="7" y="7" width="3" height="9"></rect>
          <rect x="14" y="7" width="3" height="9"></rect>
        </svg>
        Show Overlays
      `;
    }
  } else {
    const elementIds = uibridgeElements.map((el) => el.id);
    const response = await sendUIBridgeCommand("showOverlays", { elementIds });
    if (response?.success) {
      showingOverlays = true;
      showOverlaysBtn.innerHTML = `
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <rect x="3" y="3" width="18" height="18" rx="2" ry="2"></rect>
          <line x1="9" y1="9" x2="15" y2="15"></line>
          <line x1="15" y1="9" x2="9" y2="15"></line>
        </svg>
        Hide Overlays
      `;
    }
  }
}

/**
 * Highlight a specific element
 */
async function highlightElement(elementId) {
  await sendUIBridgeCommand("highlightElement", { elementId });
  showResult(true, `Highlighting: ${elementId}`);
}

/**
 * Execute an action on a UI Bridge element
 */
async function executeUIBridgeAction(elementId, action, params = {}) {
  const response = await sendUIBridgeCommand("executeAction", { elementId, action, params });

  if (response?.success) {
    showResult(true, `Action "${action}" executed on ${elementId}`);
    // Refresh elements to see state changes
    setTimeout(refreshUIBridgeElements, 500);
  } else {
    showResult(false, response?.error || `Failed to execute ${action}`);
  }
}

/**
 * View state snapshot
 */
async function viewStateSnapshot() {
  const response = await sendUIBridgeCommand("getStateSnapshot");

  if (response?.success && response?.data) {
    const snapshot = response.data;
    const info = [
      `Active States: ${snapshot.activeStates?.join(", ") || "none"}`,
      `States: ${snapshot.states?.length || 0}`,
      `Transitions: ${snapshot.transitions?.length || 0}`,
      `Timestamp: ${new Date(snapshot.timestamp).toLocaleTimeString()}`,
    ];
    showResult(true, "State Snapshot", info.join("<br>"));
  } else {
    showResult(false, "Failed to get state snapshot");
  }
}

// UI Bridge event listeners
togglePickerBtn.addEventListener("click", togglePicker);
refreshElementsBtn.addEventListener("click", refreshUIBridgeElements);
showOverlaysBtn.addEventListener("click", toggleOverlays);
viewSnapshotBtn.addEventListener("click", viewStateSnapshot);

// Listen for UI Bridge events from background
chrome.runtime.onMessage.addListener((message, _sender, _sendResponse) => {
  if (message.type === "UI_BRIDGE_EVENT") {
    const { eventType, data } = message;

    if (eventType === "elementPicked") {
      // Element was picked via the picker
      showResult(true, `Picked: ${data.id}`, `Type: ${data.type}, Tag: ${data.tagName}`);
      // Auto-disable picker after selection
      if (isPickerActive) {
        togglePicker();
      }
      // Refresh elements
      refreshUIBridgeElements();
    } else if (eventType === "elementHovered") {
      // Could show hover info in status
    }
  }
});

// =============================================================================
// Specs Functions
// =============================================================================

/**
 * Fetch and display specs from the page's SpecStore
 */
async function refreshSpecs() {
  refreshSpecsBtn.disabled = true;

  const response = await sendUIBridgeCommand("getSpecs");

  refreshSpecsBtn.disabled = false;

  if (response?.success && response?.data) {
    const specs = response.data.specs || [];
    specsCount.textContent = `${specs.length} spec${specs.length !== 1 ? "s" : ""} loaded`;
    renderSpecsList(specs);
  } else {
    specsCount.textContent = "0 specs loaded";
    specsList.innerHTML =
      '<div class="request-list-empty">Failed to fetch specs. Make sure UI Bridge is active on the page.</div>';
  }
}

/**
 * Render the specs list
 */
function renderSpecsList(specs) {
  if (specs.length === 0) {
    specsList.innerHTML =
      '<div class="request-list-empty">No specs loaded. Make sure the page has specs in its SpecStore.</div>';
    return;
  }

  const html = specs
    .map((spec) => {
      const config = spec.config || {};
      const groupCount = config.groups?.length || 0;
      const assertionCount = countAssertions(config);
      const version = config.version || "?";

      return `
        <div class="request-item spec-item" data-spec-id="${escapeHtml(spec.specId)}">
          <div class="request-main-row">
            <span class="request-method get">SPEC</span>
            <span class="request-url" title="${escapeHtml(spec.specId)}">${escapeHtml(spec.specId)}</span>
            <span class="request-host">v${escapeHtml(version)}</span>
          </div>
          <div class="request-body-preview">
            ${groupCount} group${groupCount !== 1 ? "s" : ""}, ${assertionCount} assertion${assertionCount !== 1 ? "s" : ""}${config.description ? " - " + escapeHtml(config.description) : ""}
          </div>
          <div class="request-actions">
            <button class="btn-send spec-view-btn" data-spec-id="${escapeHtml(spec.specId)}" title="View spec details">View</button>
            <button class="btn-copy spec-copy-btn" data-spec-id="${escapeHtml(spec.specId)}" title="Copy spec JSON">Copy</button>
          </div>
        </div>
      `;
    })
    .join("");

  specsList.innerHTML = html;

  // Add event listeners
  specsList.querySelectorAll(".spec-view-btn").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      viewSpecDetail(btn.dataset.specId, specs);
    });
  });

  specsList.querySelectorAll(".spec-copy-btn").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      copySpecJson(btn.dataset.specId, specs);
    });
  });
}

/**
 * Count total assertions in a spec config
 */
function countAssertions(config) {
  let count = 0;
  if (config.groups) {
    for (const group of config.groups) {
      count += group.assertions?.length || 0;
    }
  }
  if (config.assertions) {
    count += config.assertions.length;
  }
  return count;
}

/**
 * Show detail view for a spec
 */
function viewSpecDetail(specId, specs) {
  const spec = specs.find((s) => s.specId === specId);
  if (!spec) return;

  specDetail.style.display = "block";
  specDetailContent.textContent = JSON.stringify(spec.config, null, 2);
}

/**
 * Copy spec JSON to clipboard
 */
async function copySpecJson(specId, specs) {
  const spec = specs.find((s) => s.specId === specId);
  if (!spec) return;

  try {
    await navigator.clipboard.writeText(JSON.stringify(spec.config, null, 2));
    showResult(true, `Copied spec "${specId}" to clipboard`);
  } catch {
    showResult(false, "Failed to copy to clipboard");
  }
}

// Specs event listeners
refreshSpecsBtn.addEventListener("click", refreshSpecs);
closeSpecDetailBtn.addEventListener("click", () => {
  specDetail.style.display = "none";
});
