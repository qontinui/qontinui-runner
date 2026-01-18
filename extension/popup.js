/**
 * Popup Script for Qontinui DOM Capture Extension
 *
 * Handles UI interactions and communicates with the background service worker.
 */

/**
 * Safe wrapper for chrome.runtime.sendMessage that handles errors gracefully.
 * Returns null if there's an error instead of throwing.
 */
function safeSendMessage(message) {
  return new Promise((resolve) => {
    try {
      chrome.runtime.sendMessage(message, (response) => {
        if (chrome.runtime.lastError) {
          // Service worker may be inactive or reloading
          console.debug("[Qontinui] sendMessage error:", chrome.runtime.lastError.message);
          resolve(null);
          return;
        }
        resolve(response);
      });
    } catch (error) {
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
const clearRecordingsBtn = document.getElementById("clearRecordingsBtn");
const recordingIndicator = document.getElementById("recordingIndicator");
const requestCountEl = document.getElementById("requestCount");
const requestList = document.getElementById("requestList");
const tabs = document.querySelectorAll(".tab");
const recorderTab = document.getElementById("recorderTab");
const manualTab = document.getElementById("manualTab");

// State
let isRunnerAvailable = false;
let isCapturing = false;
let isRecording = false;
let refreshInterval = null;

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
        `Size: ${formatSize(response.size)}${selector ? ` | Selector: ${selector}` : " | Full page"}`
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
      showResult(
        true,
        "API request imported!",
        `${response.method} ${response.url}`
      );
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
// Tab Switching
// =============================================================================

tabs.forEach((tab) => {
  tab.addEventListener("click", () => {
    // Update tab buttons
    tabs.forEach((t) => t.classList.remove("active"));
    tab.classList.add("active");

    // Show/hide content
    const tabId = tab.dataset.tab;
    if (tabId === "recorder") {
      recorderTab.classList.add("active");
      manualTab.classList.remove("active");
    } else {
      recorderTab.classList.remove("active");
      manualTab.classList.add("active");
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
    requestList.innerHTML = '<div class="request-list-empty">Recording... perform actions in your app</div>';
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
      requestList.innerHTML = '<div class="request-list-empty">No requests captured. Click "Start Recording" to begin.</div>';
    }
    return;
  }

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
      const repeatBadge = req.repeatCount > 1
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
    } catch (error) {
      showResult(false, "Failed to copy to clipboard");
    }
  } else {
    showResult(false, response?.error || "Failed to copy cURL");
  }
}

/**
 * Clear all captured requests
 */
async function clearCapturedRequests() {
  await safeSendMessage({ action: "clearCapturedRequests" });
  requestList.innerHTML = '<div class="request-list-empty">No requests captured. Click "Start Recording" to begin.</div>';
  requestCountEl.textContent = "0";
}

// Event listeners for recorder
recordBtn.addEventListener("click", toggleRecording);
clearRecordingsBtn.addEventListener("click", clearCapturedRequests);

// Check if recording is already in progress when popup opens
safeSendMessage({ action: "getRecordingStatus" }).then((response) => {
  if (response?.isRecording) {
    updateRecordingUI(true);
    refreshRequestList();
  }
});
