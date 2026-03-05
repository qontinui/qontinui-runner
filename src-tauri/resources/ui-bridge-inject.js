/**
 * UI Bridge Runtime Injection Script
 *
 * Self-contained script that when injected into any web page:
 * 1. Scans the DOM for interactive elements
 * 2. Assigns auto-generated IDs based on content/attributes
 * 3. Tracks element state (visible, enabled, value, etc.)
 * 4. Exposes a control API at window.__uiBridge
 * 5. Observes DOM mutations for dynamic element discovery
 * 6. Supports actions: click, type, clear, select, focus, blur, hover, scroll
 */
(function () {
  "use strict";

  if (window.__uiBridge) return; // Already injected

  // =========================================================================
  // Element Registry
  // =========================================================================

  const registry = new Map(); // id -> { element, meta }
  let nextAutoId = 1;

  function generateId(el) {
    // Priority: data-ui-bridge-id > id > aria-label > name > text content > auto
    if (el.dataset.uiBridgeId) return el.dataset.uiBridgeId;
    if (el.id) return `id:${el.id}`;
    const ariaLabel = el.getAttribute("aria-label");
    if (ariaLabel) return `aria:${slugify(ariaLabel)}`;
    if (el.name) return `name:${el.name}`;
    const text = getVisibleText(el);
    if (text && text.length <= 40) return `text:${slugify(text)}`;
    return `auto:${el.tagName.toLowerCase()}-${nextAutoId++}`;
  }

  function slugify(str) {
    return str
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-|-$/g, "")
      .substring(0, 40);
  }

  function getVisibleText(el) {
    if (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT") {
      return el.placeholder || el.getAttribute("aria-label") || "";
    }
    // Get direct text content only (not nested elements' text)
    const text = Array.from(el.childNodes)
      .filter((n) => n.nodeType === Node.TEXT_NODE)
      .map((n) => n.textContent.trim())
      .join(" ")
      .trim();
    return text || el.textContent?.trim()?.substring(0, 50) || "";
  }

  // =========================================================================
  // Element Discovery
  // =========================================================================

  const INTERACTIVE_SELECTORS = [
    "button",
    "a[href]",
    "input",
    "textarea",
    "select",
    '[role="button"]',
    '[role="link"]',
    '[role="tab"]',
    '[role="menuitem"]',
    '[role="checkbox"]',
    '[role="radio"]',
    '[role="switch"]',
    '[role="slider"]',
    '[role="combobox"]',
    '[role="textbox"]',
    "[tabindex]",
    "[onclick]",
    "[contenteditable]",
    "details > summary",
    "label[for]",
  ];

  function isVisible(el) {
    if (!el.offsetParent && el.tagName !== "BODY" && el.tagName !== "HTML") {
      const style = getComputedStyle(el);
      if (style.position === "fixed" || style.position === "sticky") return true;
      return false;
    }
    const rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  }

  function getElementState(el) {
    const rect = el.getBoundingClientRect();
    return {
      visible: isVisible(el),
      enabled: !el.disabled && !el.getAttribute("aria-disabled"),
      focused: document.activeElement === el,
      checked: el.checked ?? el.getAttribute("aria-checked") === "true",
      value: el.value ?? el.textContent?.trim()?.substring(0, 200) ?? "",
      rect: {
        x: Math.round(rect.x),
        y: Math.round(rect.y),
        width: Math.round(rect.width),
        height: Math.round(rect.height),
      },
    };
  }

  function getElementMeta(el) {
    const tag = el.tagName.toLowerCase();
    let type = tag;
    if (tag === "input") type = `input:${el.type || "text"}`;
    if (el.getAttribute("role")) type = `role:${el.getAttribute("role")}`;

    return {
      tag,
      type,
      text: getVisibleText(el),
      ariaLabel: el.getAttribute("aria-label") || null,
      ariaRole: el.getAttribute("role") || null,
      name: el.name || null,
      htmlId: el.id || null,
      href: el.href || null,
      placeholder: el.placeholder || null,
      className: el.className?.toString()?.substring(0, 100) || null,
    };
  }

  function discoverElements(root = document) {
    const selector = INTERACTIVE_SELECTORS.join(", ");
    const elements = root.querySelectorAll(selector);
    let added = 0;

    for (const el of elements) {
      // Skip hidden or zero-size elements
      if (!isVisible(el)) continue;
      // Skip if already registered
      if (el.__uiBridgeId && registry.has(el.__uiBridgeId)) continue;

      const id = generateId(el);
      // Handle duplicate IDs by appending counter
      let uniqueId = id;
      let counter = 2;
      while (registry.has(uniqueId)) {
        uniqueId = `${id}-${counter++}`;
      }

      el.__uiBridgeId = uniqueId;
      registry.set(uniqueId, { element: el, meta: getElementMeta(el) });
      added++;
    }

    return added;
  }

  // =========================================================================
  // Actions
  // =========================================================================

  function executeAction(id, action, params = {}) {
    const entry = registry.get(id);
    if (!entry) return { success: false, error: `Element not found: ${id}` };

    const el = entry.element;
    if (!document.contains(el)) {
      registry.delete(id);
      return { success: false, error: `Element no longer in DOM: ${id}` };
    }

    try {
      switch (action) {
        case "click":
          el.scrollIntoView({ block: "center", behavior: "instant" });
          el.click();
          return { success: true };

        case "dblclick":
          el.scrollIntoView({ block: "center", behavior: "instant" });
          el.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
          return { success: true };

        case "type": {
          const text = params.text || params.value || "";
          el.focus();
          if ("value" in el) {
            // React needs nativeInputValueSetter for controlled components
            const nativeSetter = Object.getOwnPropertyDescriptor(
              HTMLInputElement.prototype, "value"
            )?.set || Object.getOwnPropertyDescriptor(
              HTMLTextAreaElement.prototype, "value"
            )?.set;
            if (nativeSetter) {
              nativeSetter.call(el, text);
            } else {
              el.value = text;
            }
            el.dispatchEvent(new Event("input", { bubbles: true }));
            el.dispatchEvent(new Event("change", { bubbles: true }));
          } else if (el.contentEditable === "true") {
            el.textContent = text;
            el.dispatchEvent(new Event("input", { bubbles: true }));
          }
          return { success: true };
        }

        case "clear":
          if ("value" in el) {
            const nativeSetter = Object.getOwnPropertyDescriptor(
              HTMLInputElement.prototype, "value"
            )?.set || Object.getOwnPropertyDescriptor(
              HTMLTextAreaElement.prototype, "value"
            )?.set;
            if (nativeSetter) {
              nativeSetter.call(el, "");
            } else {
              el.value = "";
            }
            el.dispatchEvent(new Event("input", { bubbles: true }));
            el.dispatchEvent(new Event("change", { bubbles: true }));
          }
          return { success: true };

        case "select": {
          const value = params.value;
          if (el.tagName === "SELECT" && value !== undefined) {
            el.value = value;
            el.dispatchEvent(new Event("change", { bubbles: true }));
          }
          return { success: true };
        }

        case "focus":
          el.focus();
          return { success: true };

        case "blur":
          el.blur();
          return { success: true };

        case "hover":
          el.dispatchEvent(new MouseEvent("mouseenter", { bubbles: true }));
          el.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
          return { success: true };

        case "scroll":
          el.scrollIntoView({
            block: params.block || "center",
            behavior: params.behavior || "smooth",
          });
          return { success: true };

        case "check":
          if (el.type === "checkbox" || el.type === "radio") {
            if (!el.checked) el.click();
          }
          return { success: true };

        case "uncheck":
          if (el.type === "checkbox") {
            if (el.checked) el.click();
          }
          return { success: true };

        default:
          return { success: false, error: `Unknown action: ${action}` };
      }
    } catch (err) {
      return { success: false, error: err.message };
    }
  }

  // =========================================================================
  // Snapshot
  // =========================================================================

  function getSnapshot() {
    // Rediscover to catch new elements
    discoverElements();

    const elements = [];
    for (const [id, entry] of registry) {
      if (!document.contains(entry.element)) {
        registry.delete(id);
        continue;
      }
      elements.push({
        id,
        ...entry.meta,
        state: getElementState(entry.element),
        actions: getAvailableActions(entry.element),
      });
    }

    return {
      url: window.location.href,
      title: document.title,
      timestamp: Date.now(),
      elementCount: elements.length,
      elements,
    };
  }

  function getAvailableActions(el) {
    const actions = ["click", "focus", "blur", "scroll", "hover"];
    const tag = el.tagName.toLowerCase();

    if (tag === "input" || tag === "textarea" || el.contentEditable === "true") {
      actions.push("type", "clear");
    }
    if (tag === "select") {
      actions.push("select");
    }
    if (el.type === "checkbox" || el.type === "radio") {
      actions.push("check", "uncheck");
    }
    if (tag === "a" || tag === "button" || el.getAttribute("role") === "button") {
      actions.push("dblclick");
    }
    return actions;
  }

  // =========================================================================
  // Mutation Observer — Dynamic Element Discovery
  // =========================================================================

  const observer = new MutationObserver((mutations) => {
    let needsRediscover = false;
    for (const m of mutations) {
      if (m.addedNodes.length > 0) {
        needsRediscover = true;
        break;
      }
    }
    if (needsRediscover) {
      // Debounce rediscovery
      clearTimeout(observer._timer);
      observer._timer = setTimeout(() => discoverElements(), 200);
    }
  });

  observer.observe(document.body || document.documentElement, {
    childList: true,
    subtree: true,
  });

  // =========================================================================
  // Control API (exposed on window and consumed by proxy)
  // =========================================================================

  const api = {
    /** Get all discovered elements with their state */
    getElements() {
      const elements = [];
      for (const [id, entry] of registry) {
        if (!document.contains(entry.element)) {
          registry.delete(id);
          continue;
        }
        elements.push({
          id,
          ...entry.meta,
          state: getElementState(entry.element),
        });
      }
      return elements;
    },

    /** Get a single element by ID */
    getElement(id) {
      const entry = registry.get(id);
      if (!entry || !document.contains(entry.element)) {
        registry.delete(id);
        return null;
      }
      return {
        id,
        ...entry.meta,
        state: getElementState(entry.element),
        actions: getAvailableActions(entry.element),
      };
    },

    /** Execute an action on an element */
    executeAction(id, action, params) {
      return executeAction(id, action, params);
    },

    /** Get a full page snapshot */
    getSnapshot() {
      return getSnapshot();
    },

    /** Re-run element discovery */
    discover() {
      const before = registry.size;
      discoverElements();
      return { added: registry.size - before, total: registry.size };
    },

    /** Get health information */
    getHealth() {
      return {
        status: "ok",
        injected: true,
        elementCount: registry.size,
        url: window.location.href,
        title: document.title,
        timestamp: Date.now(),
        version: "1.0.0",
      };
    },

    /** Get console errors captured since injection */
    getConsoleErrors() {
      return capturedErrors.slice();
    },

    /** Clear captured console errors */
    clearConsoleErrors() {
      capturedErrors.length = 0;
      return { success: true };
    },

    /** Navigate the page */
    navigate(url) {
      window.location.href = url;
      return { success: true };
    },

    /** Refresh the page */
    refresh() {
      window.location.reload();
      return { success: true };
    },

    /** Go back */
    back() {
      window.history.back();
      return { success: true };
    },

    /** Go forward */
    forward() {
      window.history.forward();
      return { success: true };
    },

    /** Get computed CSS styles for an element */
    getComputedStyles(id, properties) {
      const entry = registry.get(id);
      if (!entry || !document.contains(entry.element)) {
        registry.delete(id);
        return { success: false, error: "Element not found: " + id };
      }
      const computed = getComputedStyle(entry.element);
      const defaultProps = [
        "color", "backgroundColor", "fontSize", "fontWeight", "fontFamily",
        "borderColor", "borderWidth", "borderRadius", "padding", "margin",
        "display", "position", "opacity", "cursor", "textAlign",
        "lineHeight", "zIndex", "width", "height", "overflow",
      ];
      const props = Array.isArray(properties) ? properties : defaultProps;
      const styles = {};
      for (const prop of props) {
        styles[prop] = computed[prop] || null;
      }
      return { success: true, id, styles };
    },

    /** Get ARIA accessibility info for an element */
    getAccessibilityInfo(id) {
      const entry = registry.get(id);
      if (!entry || !document.contains(entry.element)) {
        registry.delete(id);
        return { success: false, error: "Element not found: " + id };
      }
      const el = entry.element;
      return {
        success: true,
        id,
        role: el.getAttribute("role") || el.tagName.toLowerCase(),
        ariaLabel: el.getAttribute("aria-label"),
        ariaDescribedBy: el.getAttribute("aria-describedby"),
        ariaLabelledBy: el.getAttribute("aria-labelledby"),
        ariaExpanded: el.getAttribute("aria-expanded"),
        ariaHidden: el.getAttribute("aria-hidden"),
        ariaDisabled: el.getAttribute("aria-disabled"),
        ariaChecked: el.getAttribute("aria-checked"),
        ariaSelected: el.getAttribute("aria-selected"),
        ariaRequired: el.getAttribute("aria-required"),
        ariaValueNow: el.getAttribute("aria-valuenow"),
        ariaValueMin: el.getAttribute("aria-valuemin"),
        ariaValueMax: el.getAttribute("aria-valuemax"),
        tabIndex: el.tabIndex,
      };
    },

    /** Wait for an element matching a CSS selector to appear */
    waitForElement(selector, timeoutMs) {
      const timeout = typeof timeoutMs === "number" ? timeoutMs : 5000;
      return new Promise((resolve) => {
        const existing = document.querySelector(selector);
        if (existing) {
          resolve({ success: true, found: true, selector });
          return;
        }
        const deadline = Date.now() + timeout;
        const obs = new MutationObserver(() => {
          if (document.querySelector(selector)) {
            obs.disconnect();
            resolve({ success: true, found: true, selector });
          } else if (Date.now() > deadline) {
            obs.disconnect();
            resolve({ success: false, found: false, selector, timedOut: true });
          }
        });
        obs.observe(document.documentElement, { childList: true, subtree: true });
        setTimeout(() => {
          obs.disconnect();
          resolve({ success: false, found: false, selector, timedOut: true });
        }, timeout);
      });
    },

    /** Find elements by CSS selector (not just interactive ones) */
    querySelectorAll(selector) {
      try {
        const elements = document.querySelectorAll(selector);
        const results = Array.from(elements).map((el) => {
          const rect = el.getBoundingClientRect();
          return {
            tagName: el.tagName.toLowerCase(),
            id: el.id || null,
            className: el.className?.toString()?.substring(0, 100) || null,
            text: el.textContent?.trim()?.substring(0, 100) || null,
            rect: {
              x: Math.round(rect.x), y: Math.round(rect.y),
              width: Math.round(rect.width), height: Math.round(rect.height),
            },
            visible: rect.width > 0 && rect.height > 0,
          };
        });
        return { success: true, selector, count: results.length, elements: results };
      } catch (err) {
        return { success: false, error: err.message };
      }
    },

    /** Get all elements with their computed styles (design snapshot) */
    getDesignSnapshot() {
      discoverElements();
      const elems = [];
      const defaultStyleProps = [
        "color", "backgroundColor", "fontSize", "fontWeight",
        "borderRadius", "padding", "display", "opacity",
      ];
      for (const [id, entry] of registry) {
        if (!document.contains(entry.element)) { registry.delete(id); continue; }
        const computed = getComputedStyle(entry.element);
        const styles = {};
        for (const prop of defaultStyleProps) styles[prop] = computed[prop] || null;
        elems.push({
          id,
          ...entry.meta,
          state: getElementState(entry.element),
          styles,
        });
      }
      return {
        url: window.location.href,
        title: document.title,
        timestamp: Date.now(),
        elementCount: elems.length,
        elements: elems,
      };
    },
  };

  // =========================================================================
  // Console Error Capture
  // =========================================================================

  const capturedErrors = [];
  const maxCapturedErrors = 100;
  const origConsoleError = console.error;
  console.error = function (...args) {
    if (capturedErrors.length < maxCapturedErrors) {
      capturedErrors.push({
        message: args.map((a) => (typeof a === "object" ? JSON.stringify(a) : String(a))).join(" "),
        timestamp: Date.now(),
        type: "error",
      });
    }
    origConsoleError.apply(console, args);
  };

  window.addEventListener("error", (event) => {
    if (capturedErrors.length < maxCapturedErrors) {
      capturedErrors.push({
        message: event.message,
        filename: event.filename,
        lineno: event.lineno,
        colno: event.colno,
        timestamp: Date.now(),
        type: "uncaught",
      });
    }
  });

  window.addEventListener("unhandledrejection", (event) => {
    if (capturedErrors.length < maxCapturedErrors) {
      capturedErrors.push({
        message: String(event.reason),
        timestamp: Date.now(),
        type: "unhandled_rejection",
      });
    }
  });

  // =========================================================================
  // Message-Based Communication (for proxy integration)
  // =========================================================================

  window.addEventListener("message", (event) => {
    if (event.data && event.data.__uiBridge) {
      const { method, args, requestId } = event.data;
      let result;
      try {
        if (typeof api[method] === "function") {
          result = { success: true, data: api[method](...(args || [])) };
        } else {
          result = { success: false, error: `Unknown method: ${method}` };
        }
      } catch (err) {
        result = { success: false, error: err.message };
      }
      window.postMessage(
        { __uiBridgeResponse: true, requestId, result },
        "*"
      );
    }
  });

  // =========================================================================
  // Initialize
  // =========================================================================

  window.__uiBridge = api;

  // Initial discovery
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", () => discoverElements());
  } else {
    discoverElements();
  }

  // =========================================================================
  // Proxy Control Polling — fetches pending commands from the proxy and
  // executes them against the local API, then POSTs results back.
  // =========================================================================

  setInterval(async () => {
    try {
      const resp = await fetch("/__ui-bridge/control/pending");
      if (!resp.ok) return;
      const commands = await resp.json();
      if (!commands || !commands.length) return;

      const results = [];
      for (const cmd of commands) {
        let result;
        try {
          if (typeof api[cmd.method] === "function") {
            const maybePromise = api[cmd.method](...(cmd.args || []));
            const data =
              maybePromise instanceof Promise
                ? await maybePromise
                : maybePromise;
            result = { id: cmd.id, success: true, data };
          } else {
            result = {
              id: cmd.id,
              success: false,
              error: "Unknown method: " + cmd.method,
            };
          }
        } catch (err) {
          result = { id: cmd.id, success: false, error: err.message };
        }
        results.push(result);
      }

      await fetch("/__ui-bridge/control/results", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(results),
      });
    } catch (_) {
      /* ignore polling errors — proxy may not be running */
    }
  }, 150);

  console.log(
    `[UI Bridge] Injected. Discovered ${registry.size} interactive elements.`
  );
})();
