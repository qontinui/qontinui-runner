/**
 * UI Bridge Recorder - Content Script for Manual Recording Mode
 *
 * This script captures DOM snapshots and detailed user actions during manual navigation.
 * It runs in the MAIN world to access the page's DOM directly.
 *
 * Captures are triggered by:
 * - Initial page load
 * - User clicks
 * - Input/change events
 * - Navigation (popstate, hashchange)
 * - DOM mutations (optional, can be noisy)
 * - Keyboard events (Enter, Tab, Escape)
 * - Select changes
 * - Scroll events
 *
 * Enhanced in v2:
 * - CSS selector generation
 * - XPath generation
 * - Detailed action data capture
 * - Type action text tracking with debounce
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
  let actionCount = 0;
  let mutationObserver = null;
  let lastCaptureTime = 0;
  let lastUrl = window.location.href;
  const MIN_CAPTURE_INTERVAL_MS = 100; // Debounce captures

  // Track input values for type actions
  let pendingTypeAction = null;
  let typeDebounceTimer = null;
  const TYPE_DEBOUNCE_MS = 500;

  // ============================================================================
  // Selector Generation
  // ============================================================================

  /**
   * Generate a robust CSS selector for an element
   */
  function generateCssSelector(el) {
    if (!el || el === document.body || el === document.documentElement) {
      return "body";
    }

    // Priority 1: ID attribute
    if (el.id && !el.id.match(/^\d/) && !el.id.includes(':')) {
      return `#${CSS.escape(el.id)}`;
    }

    // Priority 2: data-ui-id attribute
    const uiId = el.getAttribute("data-ui-id");
    if (uiId) {
      return `[data-ui-id="${CSS.escape(uiId)}"]`;
    }

    // Priority 3: data-testid attribute
    const testId = el.getAttribute("data-testid");
    if (testId) {
      return `[data-testid="${CSS.escape(testId)}"]`;
    }

    // Priority 4: Unique combination of tag + attributes
    const tag = el.tagName.toLowerCase();

    // For inputs, use type + name/placeholder
    if (tag === "input") {
      const type = el.getAttribute("type") || "text";
      const name = el.getAttribute("name");
      const placeholder = el.getAttribute("placeholder");

      if (name) {
        const selector = `input[type="${type}"][name="${CSS.escape(name)}"]`;
        if (isUniqueSelector(selector)) return selector;
      }
      if (placeholder) {
        const selector = `input[placeholder="${CSS.escape(placeholder)}"]`;
        if (isUniqueSelector(selector)) return selector;
      }
    }

    // For buttons/links, use text content
    if (tag === "button" || tag === "a") {
      const text = el.textContent?.trim();
      if (text && text.length < 50 && text.length > 0) {
        // Note: :has-text is Playwright syntax, for CSS we need to use different approach
        // This selector would be: `${tag}:has-text("${text.substring(0, 30)}")`
        // Return a class-based selector as fallback (handled below)
      }
    }

    // Priority 5: Class-based selector
    if (el.className && typeof el.className === "string") {
      const classes = el.className.trim().split(/\s+/).filter(c =>
        c && !c.match(/^(hover|active|focus|visited|disabled|selected)/i)
      );
      if (classes.length > 0) {
        const classSelector = classes.slice(0, 2).map(c => `.${CSS.escape(c)}`).join("");
        const selector = `${tag}${classSelector}`;
        if (isUniqueSelector(selector)) return selector;
      }
    }

    // Priority 6: Position-based selector (nth-child)
    return generatePositionalSelector(el);
  }

  /**
   * Generate a positional CSS selector using nth-child
   */
  function generatePositionalSelector(el) {
    const path = [];
    let current = el;

    while (current && current !== document.body && current !== document.documentElement) {
      const tag = current.tagName.toLowerCase();
      let selector = tag;

      // Add ID if available
      if (current.id && !current.id.match(/^\d/)) {
        selector = `#${CSS.escape(current.id)}`;
        path.unshift(selector);
        break; // ID is unique, stop here
      }

      // Add nth-child for disambiguation
      const parent = current.parentElement;
      if (parent) {
        const siblings = Array.from(parent.children).filter(s => s.tagName === current.tagName);
        if (siblings.length > 1) {
          const index = siblings.indexOf(current) + 1;
          selector = `${tag}:nth-child(${index})`;
        }
      }

      path.unshift(selector);
      current = current.parentElement;

      // Limit depth to avoid overly long selectors
      if (path.length >= 5) break;
    }

    return path.join(" > ");
  }

  /**
   * Check if a CSS selector uniquely identifies one element
   */
  function isUniqueSelector(selector) {
    try {
      const matches = document.querySelectorAll(selector);
      return matches.length === 1;
    } catch {
      return false;
    }
  }

  /**
   * Generate XPath for an element
   */
  function generateXPath(el) {
    if (!el || el === document) return "";
    if (el === document.body) return "/html/body";

    const parts = [];
    let current = el;

    while (current && current !== document) {
      if (current === document.body) {
        parts.unshift("/html/body");
        break;
      }

      let part = current.tagName.toLowerCase();

      // Add ID predicate if available
      if (current.id) {
        parts.unshift(`//${part}[@id="${current.id}"]`);
        break;
      }

      // Add position predicate
      const parent = current.parentElement;
      if (parent) {
        const siblings = Array.from(parent.children).filter(s => s.tagName === current.tagName);
        if (siblings.length > 1) {
          const index = siblings.indexOf(current) + 1;
          part = `${part}[${index}]`;
        }
      }

      parts.unshift(part);
      current = current.parentElement;
    }

    if (parts.length === 0) return "";
    if (parts[0].startsWith("//")) return parts.join("");
    return "/" + parts.join("/");
  }

  /**
   * Get element attributes for identification
   */
  function getElementAttributes(el) {
    return {
      id: el.id || null,
      class: el.className || null,
      name: el.getAttribute("name"),
      placeholder: el.getAttribute("placeholder"),
      aria_label: el.getAttribute("aria-label"),
      role: el.getAttribute("role"),
      href: el.getAttribute("href"),
      type: el.getAttribute("type"),
    };
  }

  /**
   * Build action target info for an element
   */
  function buildActionTarget(el) {
    if (!el) return null;

    const rect = el.getBoundingClientRect();

    return {
      ui_id: el.getAttribute("data-ui-id") || null,
      tag_name: el.tagName.toLowerCase(),
      selector: generateCssSelector(el),
      xpath: generateXPath(el),
      text_content: el.textContent?.trim()?.substring(0, 100) || null,
      bbox: {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
      },
      attributes: getElementAttributes(el),
    };
  }

  // ============================================================================
  // Element Collection
  // ============================================================================

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

  // ============================================================================
  // Action Capture
  // ============================================================================

  /**
   * Create an action object
   */
  function createAction(type, target, actionData = {}) {
    actionCount++;
    return {
      id: `action-${actionCount}-${Date.now()}`,
      timestamp: new Date().toISOString(),
      type: type,
      url: window.location.href,
      target: target,
      ...actionData,
    };
  }

  /**
   * Capture snapshot with optional action data
   */
  function captureSnapshot(trigger, targetElement = null, action = null) {
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
      // Include enhanced action data if present
      action: action,
    };

    console.log(`[Qontinui Recorder] Captured snapshot #${captureCount}: ${trigger} (${elements.length} elements)${action ? `, action: ${action.type}` : ''}`);

    // Send to bridge script which forwards to background
    window.postMessage({
      type: "QONTINUI_RECORDER_SNAPSHOT",
      snapshot: snapshot,
    }, "*");

    return snapshot;
  }

  // ============================================================================
  // Event Handlers
  // ============================================================================

  /**
   * Click event handler
   */
  function handleClick(event) {
    if (!isRecording) return;

    // Flush any pending type action first
    flushPendingTypeAction();

    // Find the actual clicked element or its parent with data-ui-id
    let target = event.target;
    const originalTarget = target;

    while (target && target !== document.body) {
      if (target.getAttribute?.("data-ui-id")) {
        break;
      }
      target = target.parentElement;
    }

    // Use original target if no UI Bridge element found
    const actionTarget = target !== document.body ? target : originalTarget;

    // Build action data
    const action = createAction("click", buildActionTarget(actionTarget), {
      click: {
        x: event.clientX,
        y: event.clientY,
        button: event.button === 0 ? "left" : event.button === 2 ? "right" : "middle",
        click_count: event.detail || 1,
      },
    });

    // Delay slightly to capture state after click effects
    setTimeout(() => {
      captureSnapshot("click", actionTarget, action);
    }, 50);
  }

  /**
   * Input event handler for text typing
   */
  function handleInput(event) {
    if (!isRecording) return;

    const target = event.target;

    // Only track text inputs
    if (!isTextInput(target)) return;

    // Update pending type action
    if (pendingTypeAction && pendingTypeAction.target.selector === generateCssSelector(target)) {
      // Same element, update the value
      pendingTypeAction.type_data.value = target.value;
    } else {
      // Different element or first input, create new pending action
      flushPendingTypeAction();

      pendingTypeAction = {
        target: buildActionTarget(target),
        type_data: {
          value: target.value,
          input_type: target.type || "text",
          clear_first: false,
        },
      };
    }

    // Reset debounce timer
    clearTimeout(typeDebounceTimer);
    typeDebounceTimer = setTimeout(() => {
      flushPendingTypeAction();
    }, TYPE_DEBOUNCE_MS);
  }

  /**
   * Check if element is a text input
   */
  function isTextInput(el) {
    if (!el) return false;
    const tag = el.tagName?.toLowerCase();
    if (tag === "textarea") return true;
    if (tag === "input") {
      const type = el.type?.toLowerCase() || "text";
      return ["text", "email", "password", "search", "tel", "url", "number"].includes(type);
    }
    return el.contentEditable === "true";
  }

  /**
   * Flush pending type action to snapshot
   */
  function flushPendingTypeAction() {
    if (!pendingTypeAction) return;

    // Only capture if there's actual text
    if (pendingTypeAction.type_data.value) {
      const action = createAction("type", pendingTypeAction.target, {
        type_data: pendingTypeAction.type_data,
      });

      captureSnapshot("input", null, action);
    }

    pendingTypeAction = null;
    clearTimeout(typeDebounceTimer);
  }

  /**
   * Change event handler (for selects)
   */
  function handleChange(event) {
    if (!isRecording) return;

    const target = event.target;
    const tag = target.tagName?.toLowerCase();

    // Handle select elements
    if (tag === "select") {
      flushPendingTypeAction();

      const selectedOption = target.options[target.selectedIndex];
      const action = createAction("select", buildActionTarget(target), {
        select: {
          value: target.value,
          selected_text: selectedOption?.textContent?.trim() || null,
          selected_index: target.selectedIndex,
        },
      });

      captureSnapshot("select", target, action);
    }
  }

  /**
   * Keyboard event handler
   */
  function handleKeydown(event) {
    if (!isRecording) return;

    // Only capture special keys
    const specialKeys = ["Enter", "Tab", "Escape", "Backspace", "Delete"];
    if (!specialKeys.includes(event.key) && !event.ctrlKey && !event.metaKey) {
      return;
    }

    // If Enter is pressed, flush pending type action first
    if (event.key === "Enter") {
      flushPendingTypeAction();
    }

    const action = createAction("keypress", buildActionTarget(event.target), {
      keypress: {
        key: event.key,
        code: event.code,
        ctrl_key: event.ctrlKey,
        shift_key: event.shiftKey,
        alt_key: event.altKey,
        meta_key: event.metaKey,
      },
    });

    // Only capture for Enter and Escape which typically trigger actions
    if (event.key === "Enter" || event.key === "Escape") {
      setTimeout(() => {
        captureSnapshot("keypress", event.target, action);
      }, 50);
    }
  }

  /**
   * Navigation event handler
   */
  function handleNavigation(event) {
    if (!isRecording) return;

    flushPendingTypeAction();

    const action = createAction("navigate", null, {
      navigate: {
        from_url: lastUrl,
        to_url: window.location.href,
        navigation_type: event?.type || "url_change",
      },
    });

    lastUrl = window.location.href;

    setTimeout(() => {
      captureSnapshot("navigation", null, action);
    }, 100);
  }

  /**
   * Scroll event handler (debounced)
   */
  let scrollTimeout = null;
  function handleScroll(event) {
    if (!isRecording) return;

    clearTimeout(scrollTimeout);
    scrollTimeout = setTimeout(() => {
      const target = event.target === document ? document.documentElement : event.target;
      const action = createAction("scroll", buildActionTarget(target), {
        scroll: {
          delta_x: 0,
          delta_y: 0,
          scroll_left: target.scrollLeft,
          scroll_top: target.scrollTop,
        },
      });

      captureSnapshot("scroll", target, action);
    }, 300);
  }

  // ============================================================================
  // Mutation Observer
  // ============================================================================

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

  // ============================================================================
  // Recording Control
  // ============================================================================

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
    actionCount = 0;
    lastUrl = window.location.href;
    pendingTypeAction = null;

    // Add event listeners
    document.addEventListener("click", handleClick, true);
    document.addEventListener("input", handleInput, true);
    document.addEventListener("change", handleChange, true);
    document.addEventListener("keydown", handleKeydown, true);
    window.addEventListener("popstate", handleNavigation);
    window.addEventListener("hashchange", handleNavigation);

    // Optional scroll tracking
    if (options.captureScroll) {
      window.addEventListener("scroll", handleScroll, true);
    }

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

    // Flush any pending type action
    flushPendingTypeAction();

    isRecording = false;

    // Remove event listeners
    document.removeEventListener("click", handleClick, true);
    document.removeEventListener("input", handleInput, true);
    document.removeEventListener("change", handleChange, true);
    document.removeEventListener("keydown", handleKeydown, true);
    window.removeEventListener("popstate", handleNavigation);
    window.removeEventListener("hashchange", handleNavigation);
    window.removeEventListener("scroll", handleScroll, true);

    // Stop mutation observer
    stopMutationObserver();

    return {
      success: true,
      captureCount: captureCount,
      actionCount: actionCount,
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
      actionCount: actionCount,
      url: window.location.href,
    };
  }

  // ============================================================================
  // Message Handler
  // ============================================================================

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

  console.log("[Qontinui Recorder] Content script loaded and ready (v2 with enhanced action capture)");
})();
