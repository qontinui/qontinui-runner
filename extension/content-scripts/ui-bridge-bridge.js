/**
 * UI Bridge Bridge Script (ISOLATED world)
 *
 * This content script runs in the ISOLATED world and acts as a bridge between:
 * - The MAIN world content script (ui-bridge-inspector.js)
 * - The Chrome extension popup and background scripts
 *
 * Communication flow:
 * Popup/Background <--chrome.runtime--> Bridge (ISOLATED) <--window.postMessage--> Inspector (MAIN)
 */

(function () {
  'use strict';

  // Handle extension reinstall: remove old listeners and re-register
  // The old extension context is invalidated on reinstall, so we need fresh listeners
  if (window.__QONTINUI_UI_BRIDGE_BRIDGE_INITIALIZED__) {
    // Clean up old listeners if they exist
    if (window.__qontinuiUIBridgeBridgeCleanup) {
      window.__qontinuiUIBridgeBridgeCleanup();
    }
  }
  window.__QONTINUI_UI_BRIDGE_BRIDGE_INITIALIZED__ = true;

  // Track pending requests for response correlation
  const pendingRequests = new Map();
  let requestIdCounter = 0;

  /**
   * Generate unique request ID
   */
  function generateRequestId() {
    return `bridge-${Date.now()}-${++requestIdCounter}`;
  }

  /**
   * Send a request to the MAIN world inspector and wait for response
   */
  function sendToInspector(action, params = {}) {
    return new Promise((resolve, reject) => {
      const requestId = generateRequestId();
      const timeout = setTimeout(() => {
        pendingRequests.delete(requestId);
        reject(new Error(`Request ${action} timed out`));
      }, 10000); // 10 second timeout

      pendingRequests.set(requestId, { resolve, reject, timeout });

      window.postMessage(
        {
          type: '__QONTINUI_UI_BRIDGE_REQUEST__',
          requestId,
          action,
          params,
        },
        '*'
      );
    });
  }

  /**
   * Handle responses from the MAIN world inspector
   */
  function handleInspectorResponse(event) {
    if (event.source !== window) return;
    if (!event.data || event.data.type !== '__QONTINUI_UI_BRIDGE_RESPONSE__') return;

    const { requestId, success, data, error } = event.data;
    const pending = pendingRequests.get(requestId);

    if (pending) {
      clearTimeout(pending.timeout);
      pendingRequests.delete(requestId);

      if (success) {
        pending.resolve(data);
      } else {
        pending.reject(new Error(error || 'Unknown error'));
      }
    }
  }

  /**
   * Handle events pushed from the MAIN world (picker, hover, etc.)
   */
  function handleInspectorEvent(event) {
    if (event.source !== window) return;
    if (!event.data) return;

    const { type, data } = event.data;

    // Handle different event types
    if (type === '__QONTINUI_UI_BRIDGE_EVENT__') {
      // Forward generic events to background script
      chrome.runtime.sendMessage({
        type: 'UI_BRIDGE_EVENT',
        eventType: data.eventType,
        data: data.data,
        tabId: null,
      });
    } else if (type === '__QONTINUI_ELEMENT_SELECTED__') {
      // Element was picked via the picker
      chrome.runtime.sendMessage({
        type: 'UI_BRIDGE_EVENT',
        eventType: 'elementPicked',
        data: data,
        tabId: null,
      });
    } else if (type === '__QONTINUI_PICKER_CANCELLED__') {
      // Picker was cancelled
      chrome.runtime.sendMessage({
        type: 'UI_BRIDGE_EVENT',
        eventType: 'pickerCancelled',
        data: {},
        tabId: null,
      });
    }
  }

  /**
   * Handle runner commands from the webpage (for cloud qontinui.io)
   * This allows qontinui.io to send commands to the local runner through the extension
   */
  function handleRunnerCommand(event) {
    if (event.source !== window) return;
    if (!event.data || event.data.type !== '__QONTINUI_RUNNER_COMMAND__') return;

    const { requestId, action, params } = event.data;

    // Forward to background script which will call the runner API
    chrome.runtime.sendMessage(
      {
        type: 'RUNNER_COMMAND_FROM_PAGE',
        requestId,
        action,
        params,
      },
      (response) => {
        // Send response back to the page
        window.postMessage(
          {
            type: '__QONTINUI_RUNNER_RESPONSE__',
            requestId,
            success: response?.success ?? false,
            data: response?.data,
            error: response?.error,
          },
          '*'
        );
      }
    );
  }

  /**
   * Handle messages from popup/background scripts
   */
  function handleExtensionMessage(message, sender, sendResponse) {
    if (!message || message.type !== 'UI_BRIDGE_COMMAND') {
      return false;
    }

    const { action, params } = message;

    // Handle the request asynchronously
    (async () => {
      try {
        const result = await sendToInspector(action, params);
        sendResponse({ success: true, data: result });
      } catch (error) {
        sendResponse({ success: false, error: error.message });
      }
    })();

    // Return true to indicate async response
    return true;
  }

  /**
   * Check if UI Bridge is available in the page
   */
  async function checkUIBridgeAvailable() {
    try {
      const result = await sendToInspector('ping');
      return result && result.available;
    } catch {
      return false;
    }
  }

  /**
   * Get registered elements from the page
   */
  async function getElements(options = {}) {
    return sendToInspector('getElements', options);
  }

  /**
   * Get state management snapshot
   */
  async function getStateSnapshot() {
    return sendToInspector('getStateSnapshot');
  }

  /**
   * Execute an action on an element
   */
  async function executeAction(elementId, action, params = {}) {
    return sendToInspector('executeAction', { elementId, action, params });
  }

  /**
   * Enable element picker mode
   */
  async function enablePicker() {
    return sendToInspector('enablePicker');
  }

  /**
   * Disable element picker mode
   */
  async function disablePicker() {
    return sendToInspector('disablePicker');
  }

  /**
   * Show element overlays
   */
  async function showOverlays(elementIds) {
    return sendToInspector('showOverlays', { elementIds });
  }

  /**
   * Hide element overlays
   */
  async function hideOverlays() {
    return sendToInspector('hideOverlays');
  }

  /**
   * Highlight a specific element
   */
  async function highlightElement(elementId) {
    return sendToInspector('highlightElement', { elementId });
  }

  /**
   * Clear element highlight
   */
  async function clearHighlight() {
    return sendToInspector('clearHighlight');
  }

  // Set up event listeners
  window.addEventListener('message', handleInspectorResponse);
  window.addEventListener('message', handleInspectorEvent);
  window.addEventListener('message', handleRunnerCommand);
  chrome.runtime.onMessage.addListener(handleExtensionMessage);

  // Store cleanup function for extension reinstall handling
  window.__qontinuiUIBridgeBridgeCleanup = function() {
    window.removeEventListener('message', handleInspectorResponse);
    window.removeEventListener('message', handleInspectorEvent);
    window.removeEventListener('message', handleRunnerCommand);
    chrome.runtime.onMessage.removeListener(handleExtensionMessage);
    console.log('[Qontinui UI Bridge] Old bridge listeners cleaned up for reinstall');
  };

  // Expose bridge API for debugging (only in isolated world, not accessible from page)
  window.__qontinuiUIBridgeBridge = {
    checkUIBridgeAvailable,
    getElements,
    getStateSnapshot,
    executeAction,
    enablePicker,
    disablePicker,
    showOverlays,
    hideOverlays,
    highlightElement,
    clearHighlight,
  };

  // Notify background script that bridge is ready
  chrome.runtime.sendMessage({
    type: 'UI_BRIDGE_BRIDGE_READY',
    url: window.location.href,
  });

  console.log('[Qontinui UI Bridge] Bridge script initialized (ISOLATED world)');
})();
