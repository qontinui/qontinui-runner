/**
 * UI Bridge Runtime Injection Script
 *
 * Self-contained script that when injected into any web page:
 * 1. Scans the DOM for interactive elements
 * 2. Assigns auto-generated IDs based on content/attributes
 * 3. Tracks element state (visible, enabled, value, validation, constraints)
 * 4. Exposes a control API at window.__uiBridge
 * 5. Observes DOM mutations for dynamic element discovery
 * 6. Supports actions: click, type, clear, select, focus, blur, hover, scroll
 * 7. Form state awareness: discovers forms, field states, validation errors
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
    // Priority: id > aria-label > name > text content > auto
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

  function isFormControl(el) {
    return (
      el instanceof HTMLInputElement ||
      el instanceof HTMLTextAreaElement ||
      el instanceof HTMLSelectElement
    );
  }

  function getElementState(el) {
    const rect = el.getBoundingClientRect();
    const state = {
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

    // Enrich with form control properties
    if (isFormControl(el)) {
      state.required = el.required || el.getAttribute("aria-required") === "true";

      // HTML5 constraint validation
      if ("validity" in el) {
        const v = el.validity;
        state.validationState = {
          valid: v.valid,
          validationMessage: el.validationMessage || undefined,
          valueMissing: v.valueMissing || undefined,
          typeMismatch: v.typeMismatch || undefined,
          patternMismatch: v.patternMismatch || undefined,
          tooShort: v.tooShort || undefined,
          tooLong: v.tooLong || undefined,
          rangeUnderflow: v.rangeUnderflow || undefined,
          rangeOverflow: v.rangeOverflow || undefined,
          stepMismatch: v.stepMismatch || undefined,
          customError: v.customError || undefined,
        };
      }

      // Constraint attributes
      const constraints = {};
      if (el.pattern) constraints.pattern = el.pattern;
      if (el.minLength >= 0) constraints.minLength = el.minLength;
      if (el.maxLength >= 0) constraints.maxLength = el.maxLength;
      if (el.min) constraints.min = el.min;
      if (el.max) constraints.max = el.max;
      if (el.step) constraints.step = el.step;
      if (Object.keys(constraints).length > 0) state.constraints = constraints;

      // Selected options for <select>
      if (el.tagName === "SELECT") {
        state.selectedOptions = Array.from(el.selectedOptions).map((o) => o.value);
      }
    }

    return state;
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
  // Form State Awareness — Validation Error Detection
  // =========================================================================

  /** CSS classes that indicate an error message container */
  const ERROR_CONTAINER_SELECTORS = [
    ".error", ".field-error", ".form-error", ".invalid-feedback",
    ".help-block.error", ".error-message", ".validation-error",
    ".form-text.text-danger", '[role="alert"]',
    ".MuiFormHelperText-root.Mui-error", ".ant-form-item-explain-error",
    ".chakra-form__error-message", ".text-red-500", ".text-red-600",
    ".text-destructive",
  ];

  /** CSS classes that indicate an input is in error state */
  const INPUT_ERROR_CLASSES = [
    "is-invalid", "has-error", "error", "invalid", "field-error",
    "border-red-500", "border-destructive", "Mui-error",
    "ant-input-status-error",
  ];

  function hasErrorClass(el) {
    for (const cls of INPUT_ERROR_CLASSES) {
      if (el.classList.contains(cls)) return true;
    }
    if (el.getAttribute("data-invalid") === "true" || el.getAttribute("data-error") !== null) {
      return true;
    }
    return false;
  }

  function getAriaErrorMessage(el) {
    const errorMsgId = el.getAttribute("aria-errormessage");
    if (errorMsgId) {
      const errorEl = document.getElementById(errorMsgId);
      if (errorEl?.textContent?.trim()) return errorEl.textContent.trim();
    }
    const describedById = el.getAttribute("aria-describedby");
    if (describedById) {
      const ids = describedById.split(/\s+/);
      for (const id of ids) {
        const refEl = document.getElementById(id);
        if (refEl?.textContent?.trim()) {
          const text = refEl.textContent.trim();
          if (refEl.getAttribute("role") === "alert" || hasErrorClass(refEl) || text.length < 200) {
            return text;
          }
        }
      }
    }
    return null;
  }

  function findAdjacentError(el) {
    const container = el.closest(
      ".form-group, .form-field, .field, .form-item, " +
      '[class*="field"], [class*="form-group"], [class*="FormControl"], ' +
      ".MuiFormControl-root, .ant-form-item, .chakra-form-control"
    ) || el.parentElement;
    if (!container) return null;
    for (const selector of ERROR_CONTAINER_SELECTORS) {
      try {
        const errorEl = container.querySelector(selector);
        if (errorEl && errorEl !== el) {
          const text = errorEl.textContent?.trim();
          if (text) return text;
        }
      } catch (_) { /* invalid selector */ }
    }
    const next = el.nextElementSibling;
    if (next && hasErrorClass(next)) {
      const text = next.textContent?.trim();
      if (text) return text;
    }
    return null;
  }

  /**
   * Detect a validation error for a form control using multiple strategies:
   * 1. HTML5 Constraint Validation API (confidence 1.0)
   * 2. ARIA invalid + error message (confidence 0.95)
   * 3. Adjacent error elements (confidence 0.8)
   * 4. CSS class heuristics (confidence 0.6)
   */
  function detectFieldError(el) {
    // Strategy 1: HTML5 validity
    if ("validity" in el && !el.validity.valid) {
      return { message: el.validationMessage || "Invalid value", confidence: 1.0, source: "html5" };
    }
    // Strategy 2: ARIA invalid
    if (el.getAttribute("aria-invalid") === "true") {
      return { message: getAriaErrorMessage(el) || "", confidence: 0.95, source: "aria" };
    }
    // Strategy 3: Adjacent error elements
    const adjacentError = findAdjacentError(el);
    if (adjacentError) {
      return { message: adjacentError, confidence: 0.8, source: "adjacent-element" };
    }
    // Strategy 4: CSS class on the input
    if (hasErrorClass(el)) {
      return { message: "", confidence: 0.6, source: "css-class" };
    }
    return null;
  }

  function getLabelText(el) {
    const id = el.id;
    if (id) {
      try {
        const label = document.querySelector('label[for="' + CSS.escape(id) + '"]');
        if (label?.textContent?.trim()) return label.textContent.trim();
      } catch (_) { /* CSS.escape may not be available */ }
    }
    const parentLabel = el.closest("label");
    if (parentLabel) {
      const clone = parentLabel.cloneNode(true);
      clone.querySelectorAll("input, textarea, select").forEach((inp) => inp.remove());
      const text = clone.textContent?.trim();
      if (text) return text;
    }
    return null;
  }

  function inferFormPurpose(fields) {
    const labels = fields.map((f) =>
      (f.getAttribute("aria-label") || f.getAttribute("name") || f.placeholder || "").toLowerCase()
    ).join(" ");
    if (labels.includes("email") && labels.includes("password")) {
      if (labels.includes("confirm") || labels.includes("name") || labels.includes("register")) {
        return "Registration";
      }
      return "Login";
    }
    if (labels.includes("search")) return "Search";
    if (labels.includes("address") || labels.includes("city")) return "Address";
    if (labels.includes("card") || labels.includes("payment")) return "Payment";
    if (labels.includes("contact") || labels.includes("message")) return "Contact";
    return "Form";
  }

  function buildFormField(el) {
    const detectedError = detectFieldError(el);
    const defaultValue = el.getAttribute("value") ?? "";
    const currentValue = el.value ?? "";
    const tag = el.tagName;

    return {
      id: el.__uiBridgeId || el.id || el.name || null,
      label: el.getAttribute("aria-label") || getLabelText(el) || el.placeholder || el.name || el.id || null,
      type: tag === "INPUT" ? (el.type || "text") : tag.toLowerCase(),
      value: currentValue,
      valid: detectedError ? false : true,
      error: detectedError?.message || undefined,
      errorSource: detectedError?.source || undefined,
      required: el.required || el.getAttribute("aria-required") === "true",
      touched: document.activeElement === el || currentValue.length > 0,
      placeholder: el.placeholder || undefined,
      isDirty: currentValue !== defaultValue,
      checked: el.type === "checkbox" || el.type === "radio" ? el.checked : undefined,
      selectedOptions: tag === "SELECT" ? Array.from(el.selectedOptions).map((o) => o.value) : undefined,
      constraints: (() => {
        const c = {};
        if (el.pattern) c.pattern = el.pattern;
        if (el.minLength >= 0) c.minLength = el.minLength;
        if (el.maxLength >= 0) c.maxLength = el.maxLength;
        if (el.min) c.min = el.min;
        if (el.max) c.max = el.max;
        if (el.step) c.step = el.step;
        return Object.keys(c).length > 0 ? c : undefined;
      })(),
    };
  }

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

    /** Get form state for all forms on the page */
    getForms() {
      discoverElements();
      const formSelector = "input, textarea, select";
      const forms = [];

      // Find all <form> elements on the page
      const formElements = document.querySelectorAll("form");

      if (formElements.length > 0) {
        for (const formEl of formElements) {
          const inputs = Array.from(formEl.querySelectorAll(formSelector)).filter(isFormControl);
          const fields = inputs.map(buildFormField);

          // Find submit button
          const submitBtn = formEl.querySelector(
            'button[type="submit"], input[type="submit"]'
          ) || Array.from(formEl.querySelectorAll("button")).find((btn) =>
            /submit|save|send|continue|sign in|log in/i.test(btn.textContent || "")
          );

          forms.push({
            id: formEl.__uiBridgeId || formEl.id || formEl.name || "form-" + (forms.length + 1),
            name: formEl.name || formEl.getAttribute("aria-label") || undefined,
            purpose: inferFormPurpose(inputs),
            fields,
            isValid: fields.every((f) => f.valid),
            submitButton: submitBtn?.__uiBridgeId || submitBtn?.id || undefined,
            isDirty: fields.some((f) => f.isDirty),
          });
        }
      }

      // Find orphan inputs (not inside any <form>)
      const allInputs = Array.from(document.querySelectorAll(formSelector)).filter(isFormControl);
      const inputsInForms = new Set();
      for (const formEl of formElements) {
        for (const input of formEl.querySelectorAll(formSelector)) {
          inputsInForms.add(input);
        }
      }
      const orphanInputs = allInputs.filter((el) => !inputsInForms.has(el));

      if (orphanInputs.length > 0) {
        const fields = orphanInputs.map(buildFormField);
        const submitBtn = Array.from(document.querySelectorAll("button")).find((btn) =>
          !Array.from(formElements).some((f) => f.contains(btn)) &&
          /submit|save|send|continue|sign in|log in/i.test(btn.textContent || "")
        );

        forms.push({
          id: "implicit-form",
          purpose: inferFormPurpose(orphanInputs),
          fields,
          isValid: fields.every((f) => f.valid),
          submitButton: submitBtn?.__uiBridgeId || submitBtn?.id || undefined,
          isDirty: fields.some((f) => f.isDirty),
        });
      }

      // Generate summary
      const totalFields = forms.reduce((sum, f) => sum + f.fields.length, 0);
      const totalErrors = forms.reduce((sum, f) => sum + f.fields.filter((ff) => !ff.valid).length, 0);
      const filledFields = forms.reduce((sum, f) => sum + f.fields.filter((ff) => ff.value !== "" || ff.checked).length, 0);
      const summaryParts = [forms.length + " form(s), " + totalFields + " field(s)"];
      if (filledFields > 0) summaryParts.push(filledFields + " filled");
      if (totalErrors > 0) summaryParts.push(totalErrors + " error(s)");

      return {
        forms,
        summary: summaryParts.join(", "),
        timestamp: Date.now(),
      };
    },
  };

  // =========================================================================
  // Console Error Capture (legacy format for getConsoleErrors API)
  // =========================================================================

  const capturedErrors = [];
  const maxCapturedErrors = 100;

  // =========================================================================
  // Browser Event Capture (AnyCapturedEvent-compatible format)
  //
  // Captures console errors/warns, unhandled rejections, resource errors,
  // and navigation events in the same format as the full SDK's
  // BrowserEventCapture. Events are pushed to the proxy periodically
  // so they can feed error sessions, snapshots, and streaming.
  // =========================================================================

  const browserEvents = [];
  const maxBrowserEvents = 200;
  /** Index of the first event not yet pushed to the proxy */
  let browserEventsPushCursor = 0;

  function pushBrowserEvent(event) {
    if (browserEvents.length >= maxBrowserEvents) {
      // Ring buffer: remove oldest
      browserEvents.shift();
      if (browserEventsPushCursor > 0) browserEventsPushCursor--;
    }
    browserEvents.push(event);
  }

  function extractStack(err) {
    if (!err) return undefined;
    if (typeof err === "object" && err.stack) return err.stack;
    return undefined;
  }

  // --- Console error/warn capture ---
  const origConsoleError = console.error;
  const origConsoleWarn = console.warn;

  console.error = function (...args) {
    const message = args.map((a) => (typeof a === "object" ? JSON.stringify(a) : String(a))).join(" ");
    const stack = args.length > 0 ? extractStack(args[0]) : undefined;
    const now = Date.now();

    // Legacy format
    if (capturedErrors.length < maxCapturedErrors) {
      capturedErrors.push({ message, timestamp: now, type: "error" });
    }

    // AnyCapturedEvent format
    pushBrowserEvent({
      type: "console",
      level: "error",
      message,
      stack,
      timestamp: now,
      url: window.location.href,
    });

    origConsoleError.apply(console, args);
  };

  console.warn = function (...args) {
    const message = args.map((a) => (typeof a === "object" ? JSON.stringify(a) : String(a))).join(" ");
    const now = Date.now();

    // AnyCapturedEvent format (warn events, no legacy capture for warns)
    pushBrowserEvent({
      type: "console",
      level: "warn",
      message,
      timestamp: now,
      url: window.location.href,
    });

    origConsoleWarn.apply(console, args);
  };

  // --- Uncaught errors ---
  window.addEventListener("error", (event) => {
    const now = Date.now();

    // Legacy format
    if (capturedErrors.length < maxCapturedErrors) {
      capturedErrors.push({
        message: event.message,
        filename: event.filename,
        lineno: event.lineno,
        colno: event.colno,
        timestamp: now,
        type: "uncaught",
      });
    }

    // If it's a resource error (img/script/link failed to load), capture as resource-error
    if (event.target && event.target !== window && event.target.tagName) {
      pushBrowserEvent({
        type: "resource-error",
        resourceUrl: event.target.src || event.target.href || "",
        tagName: event.target.tagName.toLowerCase(),
        timestamp: now,
        url: window.location.href,
      });
    } else {
      // JS error
      const stack = event.error ? event.error.stack : undefined;
      pushBrowserEvent({
        type: "console",
        level: "error",
        message: event.message || "Uncaught error",
        stack,
        timestamp: now,
        url: window.location.href,
      });
    }
  }, true); // useCapture=true to catch resource errors

  // --- Unhandled promise rejections ---
  window.addEventListener("unhandledrejection", (event) => {
    const message = String(event.reason);
    const stack = event.reason && typeof event.reason === "object" ? event.reason.stack : undefined;
    const now = Date.now();

    // Legacy format
    if (capturedErrors.length < maxCapturedErrors) {
      capturedErrors.push({ message, timestamp: now, type: "unhandled_rejection" });
    }

    // AnyCapturedEvent format
    pushBrowserEvent({
      type: "console",
      level: "unhandledrejection",
      message,
      stack,
      timestamp: now,
      url: window.location.href,
    });
  });

  // --- Navigation capture (History API) ---
  const origPushState = history.pushState;
  const origReplaceState = history.replaceState;

  history.pushState = function (...args) {
    const from = window.location.href;
    origPushState.apply(history, args);
    const to = window.location.href;
    if (from !== to) {
      pushBrowserEvent({
        type: "navigation",
        from,
        to,
        trigger: "pushState",
        timestamp: Date.now(),
        url: to,
      });
    }
  };

  history.replaceState = function (...args) {
    const from = window.location.href;
    origReplaceState.apply(history, args);
    const to = window.location.href;
    if (from !== to) {
      pushBrowserEvent({
        type: "navigation",
        from,
        to,
        trigger: "replaceState",
        timestamp: Date.now(),
        url: to,
      });
    }
  };

  window.addEventListener("popstate", () => {
    pushBrowserEvent({
      type: "navigation",
      from: "",
      to: window.location.href,
      trigger: "popstate",
      timestamp: Date.now(),
      url: window.location.href,
    });
  });

  // --- Resource error capture via global listener (img, script, link) ---
  // Note: The window "error" listener with useCapture=true above already
  // captures resource errors. This is intentional — no duplicate handler here.

  // --- Network error capture (fetch wrapper) ---
  const origFetch = window.fetch;
  const networkIgnorePatterns = ["/__ui-bridge/"];

  window.fetch = function (input, init) {
    const requestUrl = typeof input === "string" ? input : (input instanceof Request ? input.url : String(input));

    // Skip our own polling requests
    if (networkIgnorePatterns.some((p) => requestUrl.includes(p))) {
      return origFetch.apply(window, arguments);
    }

    const method = (init && init.method) || "GET";
    const startTime = Date.now();

    return origFetch.apply(window, arguments).then(
      (response) => {
        if (response.status >= 400) {
          pushBrowserEvent({
            type: "network",
            method: method.toUpperCase(),
            requestUrl,
            status: response.status,
            statusText: response.statusText || "",
            durationMs: Date.now() - startTime,
            kind: "http-error",
            timestamp: startTime,
            url: window.location.href,
          });
        }
        return response;
      },
      (error) => {
        const kind = error && error.name === "AbortError" ? "abort"
          : error && error.message && error.message.includes("timeout") ? "timeout"
          : "network-error";
        pushBrowserEvent({
          type: "network",
          method: method.toUpperCase(),
          requestUrl,
          durationMs: Date.now() - startTime,
          kind,
          errorMessage: error ? error.message : "Network error",
          timestamp: startTime,
          url: window.location.href,
        });
        throw error;
      }
    );
  };

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
  // Also pushes new browser events to the proxy for error sessions,
  // snapshots, and streaming.
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

  // Push new browser events to the proxy every 300ms
  setInterval(async () => {
    try {
      if (browserEventsPushCursor >= browserEvents.length) return;
      const newEvents = browserEvents.slice(browserEventsPushCursor);
      browserEventsPushCursor = browserEvents.length;

      await fetch("/__ui-bridge/control/browser-events/push", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(newEvents),
      });
    } catch (_) {
      /* ignore push errors — proxy may not be running */
    }
  }, 300);

  console.log(
    `[UI Bridge] Injected. Discovered ${registry.size} interactive elements.`
  );
})();
