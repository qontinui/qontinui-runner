/**
 * Bridge content script (ISOLATED world) that receives messages from the
 * request-interceptor.js (MAIN world) and forwards them to the background script.
 *
 * This is necessary because MAIN world scripts can't access chrome.runtime APIs.
 */

(function() {
  'use strict';

  // Listen for messages from the MAIN world interceptor
  window.addEventListener('message', function(event) {
    // Only accept messages from the same window
    if (event.source !== window) return;

    // Check for our specific message type
    if (event.data && event.data.type === '__QONTINUI_REQUEST_BODY__') {
      try {
        // Forward to background script (fire-and-forget, no response expected)
        chrome.runtime.sendMessage({
          action: 'interceptedRequestBody',
          data: event.data.data
        }, () => {
          // Ignore any errors - this callback prevents "message port closed" warnings
          // when the background script doesn't send a response
          if (chrome.runtime.lastError) {
            // Silently ignore - this is expected when service worker is inactive
          }
        });
      } catch (e) {
        // Extension context may be invalidated (e.g., extension reloaded)
        // This is normal, just ignore
      }
    }
  });
})();
