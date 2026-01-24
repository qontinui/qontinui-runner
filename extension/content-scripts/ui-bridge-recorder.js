/**
 * UI Bridge Recorder - Content Script for Manual Recording Mode
 *
 * This script captures DOM snapshots during manual navigation.
 * It runs in the MAIN world to access the page's DOM directly.
 *
 * Captures are triggered by:
 * - Initial page load
 * - User clicks
 * - Input/change events
 * - Navigation (popstate, hashchange)
 * - DOM mutations (optional, can be noisy)
 */

(function() {
  // Avoid double-injection
  if (window.__QONTINUI_RECORDER_INJECTED__) {
    console.log("[Qontinui Recorder] Already injected, skipping");
    return;
  }
  window.__QONTINUI_RECORDER_INJECTED__ = true;

  let isRecording = false;
  let captureCount = 0;
  let mutationObserver = null;
  let lastCaptureTime = 0;
  const MIN_CAPTURE_INTERVAL_MS = 100; // Debounce captures

  /**
   * Get all elements with data-ui-id attribute
   */
  function getUIBridgeElements() {
    const elements = document.querySelectorAll("[data-ui-id]");
    return Array.from(elements).map(el => {
      const rect = el.getBoundingClientRect();
      const computedStyle = window.getComputedStyle(el);

      return {
        id: el.getAttribute("data-ui-id"),
        tagName: el.tagName.toLowerCase(),
        textContent: el.textContent?.trim()?.substring(0, 200) || null,
        attributes: {
          class: el.className || null,
          type: el.getAttribute("type"),
          role: el.getAttribute("role"),
          ariaLabel: el.getAttribute("aria-label"),
          href: el.getAttribute("href"),
          name: el.getAttribute("name"),
          placeholder: el.getAttribute("placeholder"),
        },
        bbox: {
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
        },
        isVisible: rect.width > 0 && rect.height > 0 && computedStyle.visibility !== "hidden" && computedStyle.display !== "none",
        isEnabled: !el.disabled && !el.hasAttribute("disabled"),
        value: el.value !== undefined ? el.value : null,
      };
    });
  }

  /**
   * Capture current DOM state
   */
  function captureSnapshot(trigger, targetElement = null) {
    // Debounce rapid captures
    const now = Date.now();
    if (now - lastCaptureTime < MIN_CAPTURE_INTERVAL_MS) {
      return null;
    }
    lastCaptureTime = now;

    captureCount++;
    const elements = getUIBridgeElements();

    const snapshot = {
      id: `snapshot-${captureCount}-${now}`,
      timestamp: new Date().toISOString(),
      url: window.location.href,
      title: document.title,
      trigger: trigger,
      triggerElement: targetElement ? {
        id: targetElement.getAttribute?.("data-ui-id") || null,
        tagName: targetElement.tagName?.toLowerCase() || null,
        textContent: targetElement.textContent?.trim()?.substring(0, 100) || null,
      } : null,
      elements: elements,
      elementCount: elements.length,
    };

    console.log(`[Qontinui Recorder] Captured snapshot #${captureCount}: ${trigger} (${elements.length} elements)`);

    // Send to bridge script which forwards to background
    window.postMessage({
      type: "QONTINUI_RECORDER_SNAPSHOT",
      snapshot: snapshot,
    }, "*");

    return snapshot;
  }

  /**
   * Click event handler
   */
  function handleClick(event) {
    if (!isRecording) return;

    // Find the actual clicked element or its parent with data-ui-id
    let target = event.target;
    while (target && target !== document.body) {
      if (target.getAttribute?.("data-ui-id")) {
        break;
      }
      target = target.parentElement;
    }

    // Delay slightly to capture state after click effects
    setTimeout(() => {
      captureSnapshot("click", target);
    }, 50);
  }

  /**
   * Input/change event handler
   */
  function handleInput(event) {
    if (!isRecording) return;

    // Debounce input events more aggressively
    clearTimeout(handleInput._timeout);
    handleInput._timeout = setTimeout(() => {
      captureSnapshot("input", event.target);
    }, 300);
  }

  /**
   * Navigation event handler
   */
  function handleNavigation(event) {
    if (!isRecording) return;

    setTimeout(() => {
      captureSnapshot("navigation", null);
    }, 100);
  }

  /**
   * Start mutation observer for DOM changes
   */
  function startMutationObserver() {
    if (mutationObserver) return;

    let mutationTimeout = null;

    mutationObserver = new MutationObserver((mutations) => {
      if (!isRecording) return;

      // Check if any mutation added elements with data-ui-id
      const hasRelevantChanges = mutations.some(mutation => {
        if (mutation.type === "childList") {
          return Array.from(mutation.addedNodes).some(node => {
            if (node.nodeType !== Node.ELEMENT_NODE) return false;
            return node.hasAttribute?.("data-ui-id") ||
                   node.querySelector?.("[data-ui-id]");
          });
        }
        if (mutation.type === "attributes" && mutation.attributeName === "data-ui-id") {
          return true;
        }
        return false;
      });

      if (hasRelevantChanges) {
        // Debounce mutation captures
        clearTimeout(mutationTimeout);
        mutationTimeout = setTimeout(() => {
          captureSnapshot("mutation", null);
        }, 200);
      }
    });

    mutationObserver.observe(document.body, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["data-ui-id"],
    });
  }

  /**
   * Stop mutation observer
   */
  function stopMutationObserver() {
    if (mutationObserver) {
      mutationObserver.disconnect();
      mutationObserver = null;
    }
  }

  /**
   * Start recording
   */
  function startRecording(options = {}) {
    if (isRecording) {
      console.log("[Qontinui Recorder] Already recording");
      return { success: false, error: "Already recording" };
    }

    console.log("[Qontinui Recorder] Starting recording...");
    isRecording = true;
    captureCount = 0;

    // Add event listeners
    document.addEventListener("click", handleClick, true);
    document.addEventListener("input", handleInput, true);
    document.addEventListener("change", handleInput, true);
    window.addEventListener("popstate", handleNavigation);
    window.addEventListener("hashchange", handleNavigation);

    // Start mutation observer if requested
    if (options.captureMutations !== false) {
      startMutationObserver();
    }

    // Capture initial state
    const initialSnapshot = captureSnapshot("initial", null);

    return {
      success: true,
      initialSnapshot: initialSnapshot,
      message: "Recording started"
    };
  }

  /**
   * Stop recording
   */
  function stopRecording() {
    if (!isRecording) {
      console.log("[Qontinui Recorder] Not recording");
      return { success: false, error: "Not recording" };
    }

    console.log("[Qontinui Recorder] Stopping recording...");
    isRecording = false;

    // Remove event listeners
    document.removeEventListener("click", handleClick, true);
    document.removeEventListener("input", handleInput, true);
    document.removeEventListener("change", handleInput, true);
    window.removeEventListener("popstate", handleNavigation);
    window.removeEventListener("hashchange", handleNavigation);

    // Stop mutation observer
    stopMutationObserver();

    return {
      success: true,
      captureCount: captureCount,
      message: "Recording stopped"
    };
  }

  /**
   * Get recording status
   */
  function getRecordingStatus() {
    return {
      isRecording: isRecording,
      captureCount: captureCount,
      url: window.location.href,
    };
  }

  // Listen for commands from the bridge script
  window.addEventListener("message", (event) => {
    if (event.source !== window) return;
    if (event.data?.type !== "QONTINUI_RECORDER_COMMAND") return;

    const { action, params, requestId } = event.data;
    let result;

    switch (action) {
      case "startRecording":
        result = startRecording(params || {});
        break;
      case "stopRecording":
        result = stopRecording();
        break;
      case "getStatus":
        result = getRecordingStatus();
        break;
      case "captureNow":
        result = captureSnapshot("manual", null);
        break;
      default:
        result = { success: false, error: `Unknown action: ${action}` };
    }

    // Send response back
    window.postMessage({
      type: "QONTINUI_RECORDER_RESPONSE",
      requestId: requestId,
      result: result,
    }, "*");
  });

  console.log("[Qontinui Recorder] Content script loaded and ready");
})();
