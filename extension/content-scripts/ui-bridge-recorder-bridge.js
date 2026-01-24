/**
 * UI Bridge Recorder Bridge - Content Script (ISOLATED world)
 *
 * This script bridges communication between:
 * - The recorder script (MAIN world)
 * - The background service worker
 *
 * It forwards commands to the recorder and snapshots back to the background.
 */

(function() {
  // Avoid double-injection
  if (window.__QONTINUI_RECORDER_BRIDGE_INJECTED__) {
    return;
  }
  window.__QONTINUI_RECORDER_BRIDGE_INJECTED__ = true;

  // Pending requests waiting for responses from the recorder
  const pendingRequests = new Map();
  let requestIdCounter = 0;

  /**
   * Send a command to the recorder in the MAIN world
   */
  function sendToRecorder(action, params = {}) {
    return new Promise((resolve, reject) => {
      const requestId = `recorder-${++requestIdCounter}`;

      // Set up timeout
      const timeout = setTimeout(() => {
        pendingRequests.delete(requestId);
        reject(new Error("Recorder command timed out"));
      }, 10000);

      pendingRequests.set(requestId, { resolve, reject, timeout });

      window.postMessage({
        type: "QONTINUI_RECORDER_COMMAND",
        action: action,
        params: params,
        requestId: requestId,
      }, "*");
    });
  }

  // Listen for responses from the recorder
  window.addEventListener("message", (event) => {
    if (event.source !== window) return;

    // Handle recorder responses
    if (event.data?.type === "QONTINUI_RECORDER_RESPONSE") {
      const { requestId, result } = event.data;
      const pending = pendingRequests.get(requestId);
      if (pending) {
        clearTimeout(pending.timeout);
        pendingRequests.delete(requestId);
        pending.resolve(result);
      }
      return;
    }

    // Handle snapshots from the recorder - forward to background
    if (event.data?.type === "QONTINUI_RECORDER_SNAPSHOT") {
      const { snapshot } = event.data;
      chrome.runtime.sendMessage({
        type: "RECORDER_SNAPSHOT",
        snapshot: snapshot,
      });
      return;
    }
  });

  // Listen for commands from the background script
  chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    if (message.type !== "RECORDER_COMMAND") {
      return false;
    }

    const { action, params } = message;

    sendToRecorder(action, params)
      .then(result => {
        sendResponse({ success: true, data: result });
      })
      .catch(error => {
        sendResponse({ success: false, error: error.message });
      });

    // Return true to indicate async response
    return true;
  });

  // Notify background that bridge is ready
  chrome.runtime.sendMessage({
    type: "RECORDER_BRIDGE_READY",
    url: window.location.href,
  });

  console.log("[Qontinui Recorder Bridge] Ready");
})();
