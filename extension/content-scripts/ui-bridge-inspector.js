/**
 * UI Bridge Inspector Content Script (MAIN World)
 *
 * Provides element inspection, picking, and overlay functionality
 * for UI Bridge elements in the page.
 */

(function () {
  "use strict";

  // Handle extension reinstall: remove old listeners and re-register
  // The old extension context is invalidated on reinstall, so we need fresh listeners
  if (window.__QONTINUI_UI_BRIDGE_INSPECTOR__) {
    // Clean up old message listener if it exists
    if (window.__qontinuiUIBridgeInspectorCleanup) {
      window.__qontinuiUIBridgeInspectorCleanup();
    }
  }
  window.__QONTINUI_UI_BRIDGE_INSPECTOR__ = true;

  // State
  let pickerEnabled = false;
  let _overlayEnabled = false;
  let hoveredElement = null;
  let _selectedElement = null;
  let hoverOverlay = null;
  const overlayElements = new Map();

  // ==========================================================================
  // Cross-Origin Iframe Detection
  // ==========================================================================

  /**
   * Check if an iframe is cross-origin (cannot access its content).
   * @param {HTMLIFrameElement} iframe - The iframe element to check
   * @returns {boolean} True if the iframe is cross-origin
   */
  function isCrossOriginIframe(iframe) {
    try {
      // Try to access the contentDocument - this will throw or return null for cross-origin
      const doc = iframe.contentDocument || iframe.contentWindow?.document;
      return !doc; // If we can't access it, it's cross-origin
    } catch {
      // SecurityError = cross-origin
      return true;
    }
  }

  /**
   * Check if an element is inside a cross-origin iframe.
   * Walks up the frame hierarchy to detect cross-origin boundaries.
   * @param {Element} el - The element to check
   * @returns {boolean} True if element is inside a cross-origin iframe
   */
  function isInsideCrossOriginIframe(el) {
    try {
      // Get the element's ownerDocument
      const doc = el.ownerDocument;
      if (!doc) return false;

      // Get the window containing this document
      const win = doc.defaultView;
      if (!win) return false;

      // Check if we're in the top window
      if (win === window.top) {
        return false;
      }

      // We're in a frame - check if parent can access us
      // If the parent can't access this frame, it's cross-origin from parent's perspective
      // But if we're running this script, we have access to the current frame
      // So we need to check if the parent frame is cross-origin relative to top
      try {
        // Try to access parent window
        const parentWin = win.parent;
        if (!parentWin) return true;

        // Try to access parent document - will throw if cross-origin
        // Access the document to trigger cross-origin check (result intentionally unused)
        void parentWin.document;
        return false;
      } catch {
        // Can't access parent - we're in a cross-origin iframe
        return true;
      }
    } catch {
      // Any error suggests cross-origin restrictions
      return true;
    }
  }

  /**
   * Find all iframes on the page and return information about cross-origin ones.
   * @returns {Array<{element: HTMLIFrameElement, bounds: DOMRect, isCrossOrigin: boolean}>}
   */
  function getIframeInfo() {
    // eslint-disable-line no-unused-vars
    const iframes = document.querySelectorAll("iframe");
    return Array.from(iframes).map((iframe) => {
      const rect = iframe.getBoundingClientRect();
      return {
        element: iframe,
        bounds: {
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
          top: rect.top,
          right: rect.right,
          bottom: rect.bottom,
          left: rect.left,
        },
        isCrossOrigin: isCrossOriginIframe(iframe),
        src: iframe.src || "",
        name: iframe.name || "",
        id: iframe.id || "",
      };
    });
  }

  // Style constants
  const OVERLAY_COLOR = "rgba(59, 130, 246, 0.3)";
  const OVERLAY_BORDER = "rgba(59, 130, 246, 0.8)";
  const HOVER_COLOR = "rgba(59, 130, 246, 0.2)";
  const LABEL_BG = "rgba(59, 130, 246, 0.9)";

  /**
   * Check if an element is interactive
   * Returns true for elements that users can interact with
   */
  function isInteractive(el) {
    const tag = el.tagName.toLowerCase();

    // Check native interactive elements
    if (["button", "input", "select", "textarea", "a"].includes(tag)) {
      // Links need href to be interactive
      if (tag === "a" && !el.hasAttribute("href")) {
        return false;
      }
      return true;
    }

    // Check ARIA roles that indicate interactivity
    const role = el.getAttribute("role");
    const interactiveRoles = [
      "button",
      "link",
      "checkbox",
      "radio",
      "switch",
      "tab",
      "menuitem",
      "menuitemcheckbox",
      "menuitemradio",
      "option",
      "combobox",
      "listbox",
      "slider",
      "spinbutton",
      "textbox",
      "searchbox",
      "treeitem",
      "gridcell",
    ];
    if (role && interactiveRoles.includes(role)) {
      return true;
    }

    // Check tabindex >= 0
    const tabindex = el.getAttribute("tabindex");
    if (tabindex !== null && parseInt(tabindex, 10) >= 0) {
      return true;
    }

    // Check onclick handler
    if (el.hasAttribute("onclick") || el.onclick) {
      return true;
    }

    // Check contenteditable
    if (el.isContentEditable || el.getAttribute("contenteditable") === "true") {
      return true;
    }

    return false;
  }

  /**
   * Get all registered UI Bridge elements (with data-ui-id)
   */
  function getRegisteredElements() {
    const elements = document.querySelectorAll("[data-ui-id]");
    return Array.from(elements).map((el) => {
      const rect = el.getBoundingClientRect();
      const accessibility = getElementAccessibility(el);
      const interactive = isInteractive(el);
      const extraction = getExtractionAttributes(el);

      // Check required: HTML attribute or aria-required
      const htmlRequired = el.hasAttribute("required") || el.required === true;
      const ariaRequired = el.getAttribute("aria-required") === "true";

      // Check readonly: HTML attribute or aria-readonly
      const htmlReadonly = el.hasAttribute("readonly") || el.readOnly === true;
      const ariaReadonly = el.getAttribute("aria-readonly") === "true";

      // Check if element is inside a cross-origin iframe
      const isCrossOrigin = isInsideCrossOriginIframe(el);

      return {
        id: el.dataset.uiId,
        tagName: el.tagName.toLowerCase(),
        type: inferElementType(el),
        bounds: {
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
          top: rect.top,
          right: rect.right,
          bottom: rect.bottom,
          left: rect.left,
        },
        visible: isElementVisible(el),
        enabled: !isElementDisabled(el),
        focused: document.activeElement === el,
        value: "value" in el ? el.value : undefined,
        checked: "checked" in el ? el.checked : undefined,
        text: el.textContent?.trim().slice(0, 100) || undefined,
        label: getElementLabel(el),
        parent: findParentUiId(el),
        children: getChildUiIds(el),
        actions: inferActions(inferElementType(el)),
        hasUiId: true,
        // Cross-origin iframe detection
        isCrossOrigin: isCrossOrigin || undefined,
        // Accessibility information
        accessibility: accessibility,
        // Interactive flag
        is_interactive: interactive,
        // Flattened accessibility properties
        role: accessibility.role,
        accessibleName: accessibility.accessibleName,
        is_expanded: accessibility.ariaExpanded,
        is_pressed: accessibility.ariaPressed,
        is_selected: accessibility.ariaSelected,
        is_required: htmlRequired || ariaRequired || undefined,
        is_readonly: htmlReadonly || ariaReadonly || undefined,
        // Extraction attributes
        ...extraction,
      };
    });
  }

  /**
   * Generate a unique ID for an element that doesn't have data-ui-id.
   * Uses multiple strategies to create a stable, unique identifier.
   */
  function generateElementId(el, index) {
    // FIRST: Check for data-ui-id (highest priority - user-defined semantic ID)
    const dataUiId = el.dataset?.uiId || el.getAttribute("data-ui-id");
    if (dataUiId) {
      return dataUiId;
    }

    // Prefer existing id attribute (but only if no data-ui-id)
    if (el.id) {
      return el.id;
    }

    // Use name attribute for form elements
    if (el.name) {
      return `name-${el.name}`;
    }

    // Use aria-label if available
    const ariaLabel = el.getAttribute("aria-label");
    if (ariaLabel) {
      const sanitized = ariaLabel
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, "-")
        .slice(0, 30);
      return `aria-${sanitized}`;
    }

    // Use text content for buttons/links (sanitized)
    const text = el.textContent?.trim();
    if (text && ["button", "a"].includes(el.tagName.toLowerCase())) {
      const sanitized = text
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, "-")
        .slice(0, 30);
      return `${el.tagName.toLowerCase()}-${sanitized}`;
    }

    // Fallback: tag + index
    return `${el.tagName.toLowerCase()}-${index}`;
  }

  /**
   * Get all interactive elements on the page, including those without data-ui-id.
   * This enables AI-driven flow exploration on any website.
   */
  function getAllInteractiveElements() {
    // First get UI Bridge elements (with data-ui-id)
    const uiBridgeElements = getRegisteredElements();

    // If we have UI Bridge elements, add refs and return those (instrumented app)
    if (uiBridgeElements.length > 0) {
      // Sort by interactivity first, then add refs
      const sortedElements = [...uiBridgeElements].sort((a, b) => {
        // Interactive elements get lower ref numbers
        if (a.is_interactive && !b.is_interactive) return -1;
        if (!a.is_interactive && b.is_interactive) return 1;
        return 0;
      });

      // Add refs
      return sortedElements.map((el, index) => ({
        ...el,
        ref: `@e${index + 1}`,
      }));
    }

    // Otherwise, discover interactive elements on the page
    // Selectors for common interactive elements
    const interactiveSelectors = [
      "button",
      "a[href]",
      'input:not([type="hidden"])',
      "select",
      "textarea",
      '[role="button"]',
      '[role="link"]',
      '[role="tab"]',
      '[role="menuitem"]',
      '[role="checkbox"]',
      '[role="radio"]',
      '[role="switch"]',
      '[role="option"]',
      '[tabindex]:not([tabindex="-1"])',
      "[onclick]",
      "[data-action]",
      "[data-click]",
    ];

    const selector = interactiveSelectors.join(", ");
    const allElements = document.querySelectorAll(selector);

    // Track which elements we've already processed (to avoid duplicates)
    const seen = new Set();
    const discoveredElements = [];

    Array.from(allElements).forEach((el, index) => {
      // Skip hidden elements
      if (!isElementVisible(el)) return;

      // Skip elements we've already processed (e.g., element matching multiple selectors)
      if (seen.has(el)) return;
      seen.add(el);

      const rect = el.getBoundingClientRect();
      const generatedId = generateElementId(el, index);
      const accessibility = getElementAccessibility(el);
      const interactive = isInteractive(el);
      const extraction = getExtractionAttributes(el);

      // Check required: HTML attribute or aria-required
      const htmlRequired = el.hasAttribute("required") || el.required === true;
      const ariaRequired = el.getAttribute("aria-required") === "true";

      // Check readonly: HTML attribute or aria-readonly
      const htmlReadonly = el.hasAttribute("readonly") || el.readOnly === true;
      const ariaReadonly = el.getAttribute("aria-readonly") === "true";

      // Check if element is inside a cross-origin iframe
      const isCrossOrigin = isInsideCrossOriginIframe(el);

      discoveredElements.push({
        id: generatedId,
        tagName: el.tagName.toLowerCase(),
        type: inferElementType(el),
        bounds: {
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
          top: rect.top,
          right: rect.right,
          bottom: rect.bottom,
          left: rect.left,
        },
        visible: true, // Already filtered above
        enabled: !isElementDisabled(el),
        focused: document.activeElement === el,
        value: "value" in el ? el.value : undefined,
        checked: "checked" in el ? el.checked : undefined,
        text: el.textContent?.trim().slice(0, 100) || undefined,
        label: getElementLabel(el),
        parent: null,
        children: [],
        actions: inferActions(inferElementType(el)),
        hasUiId: false,
        // Cross-origin iframe detection
        isCrossOrigin: isCrossOrigin || undefined,
        // Accessibility information
        accessibility: accessibility,
        // Interactive flag
        is_interactive: interactive,
        // Flattened accessibility properties
        role: accessibility.role,
        accessibleName: accessibility.accessibleName,
        is_expanded: accessibility.ariaExpanded,
        is_pressed: accessibility.ariaPressed,
        is_selected: accessibility.ariaSelected,
        is_required: htmlRequired || ariaRequired || undefined,
        is_readonly: htmlReadonly || ariaReadonly || undefined,
        // Flag to indicate this is a discovered element, not UI Bridge instrumented
        _discovered: true,
        // Extraction attributes (includes selector, xpath, styles, etc.)
        ...extraction,
        // Store element reference for sorting
        _el: el,
      });
    });

    // Sort by interactivity: interactive elements get lower ref numbers
    discoveredElements.sort((a, b) => {
      if (a.is_interactive && !b.is_interactive) return -1;
      if (!a.is_interactive && b.is_interactive) return 1;
      return 0;
    });

    // Add refs and remove internal _el reference
    return discoveredElements.map((el, index) => {
      const { _el, ...rest } = el;
      return {
        ...rest,
        ref: `@e${index + 1}`,
      };
    });
  }

  /**
   * Discover visible non-interactive elements (headings, badges, labels, etc.).
   * Used when specs need to verify structural/non-interactive page elements.
   */
  function discoverNonInteractiveElements() {
    const selectors = [
      "h1",
      "h2",
      "h3",
      "h4",
      "h5",
      "h6",
      "p",
      '[role="heading"]',
      '[role="banner"]',
      '[role="status"]',
      '[role="alert"]',
      '[role="note"]',
      '[role="img"]',
      '[role="separator"]',
      "label",
      "figcaption",
      "caption",
      "legend",
    ];

    const selector = selectors.join(", ");
    const candidateElements = document.querySelectorAll(selector);
    const seen = new Set();
    const results = [];

    Array.from(candidateElements).forEach((el, index) => {
      if (!isElementVisible(el)) return;
      if (seen.has(el)) return;
      // Skip if already has data-ui-id (already covered by registered elements)
      if (el.dataset && el.dataset.uiId) return;
      seen.add(el);

      const rect = el.getBoundingClientRect();
      const accessibility = getElementAccessibility(el);
      const generatedId = generateElementId(el, 10000 + index);

      results.push({
        id: generatedId,
        tagName: el.tagName.toLowerCase(),
        type: "text",
        bounds: {
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
          top: rect.top,
          right: rect.right,
          bottom: rect.bottom,
          left: rect.left,
        },
        visible: true,
        enabled: true,
        focused: false,
        text: el.textContent?.trim().slice(0, 200) || undefined,
        label: el.getAttribute("aria-label") || undefined,
        parent: null,
        children: [],
        actions: [],
        hasUiId: false,
        is_interactive: false,
        role: accessibility.role,
        accessibleName: accessibility.accessibleName,
        _discovered: true,
      });
    });

    // Also find visible spans/divs with short text that may be badges
    document.querySelectorAll("span, div").forEach((el) => {
      if (!isElementVisible(el)) return;
      if (seen.has(el)) return;
      if (el.dataset && el.dataset.uiId) return;
      const text = el.textContent?.trim();
      // Only include text elements up to 100 chars (badges, status messages, empty states)
      if (!text || text.length > 100 || text.length === 0) return;
      // Skip if it has interactive children
      if (el.querySelector("button, a, input, select, textarea")) return;
      // Skip if parent is already captured
      if (el.closest("h1, h2, h3, h4, h5, h6, button, a, input, label")) return;

      seen.add(el);
      const rect = el.getBoundingClientRect();
      const accessibility = getElementAccessibility(el);
      const generatedId = generateElementId(el, 20000 + results.length);

      results.push({
        id: generatedId,
        tagName: el.tagName.toLowerCase(),
        type: "text",
        bounds: {
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
          top: rect.top,
          right: rect.right,
          bottom: rect.bottom,
          left: rect.left,
        },
        visible: true,
        enabled: true,
        focused: false,
        text: text.slice(0, 200),
        label: el.getAttribute("aria-label") || undefined,
        parent: null,
        children: [],
        actions: [],
        hasUiId: false,
        is_interactive: false,
        role: accessibility.role,
        accessibleName: accessibility.accessibleName,
        _discovered: true,
      });
    });

    return results;
  }

  /**
   * Build a unique CSS selector for an element.
   * Used to re-find discovered elements during executeAction.
   */
  function buildUniqueSelector(el) {
    // If element has an ID, use it
    if (el.id) {
      return `#${CSS.escape(el.id)}`;
    }

    // Build a path selector
    const parts = [];
    let current = el;
    while (current && current !== document.body) {
      let selector = current.tagName.toLowerCase();

      // Add classes if available (use first 2 classes to avoid overly specific selectors)
      const classes = Array.from(current.classList).slice(0, 2);
      if (classes.length > 0) {
        selector += "." + classes.map((c) => CSS.escape(c)).join(".");
      }

      // Add nth-child if needed for uniqueness
      const parent = current.parentElement;
      if (parent) {
        const siblings = Array.from(parent.children).filter((s) => s.tagName === current.tagName);
        if (siblings.length > 1) {
          const index = siblings.indexOf(current) + 1;
          selector += `:nth-of-type(${index})`;
        }
      }

      parts.unshift(selector);
      current = parent;

      // Limit depth to avoid overly long selectors
      if (parts.length >= 5) break;
    }

    return parts.join(" > ");
  }

  /**
   * Find an element by its ID or selector.
   * Works with both data-ui-id elements and discovered elements.
   */
  function findElementById(elementId) {
    // First try data-ui-id
    let el = document.querySelector(`[data-ui-id="${elementId}"]`);
    if (el) return el;

    // Try as native ID
    el = document.getElementById(elementId);
    if (el) return el;

    // Try as selector (for discovered elements)
    try {
      // Handle special generated IDs
      if (elementId.startsWith("name-")) {
        const name = elementId.slice(5);
        el = document.querySelector(`[name="${CSS.escape(name)}"]`);
        if (el) return el;
      }

      if (elementId.startsWith("aria-")) {
        // Search by aria-label prefix match
        const searchText = elementId.slice(5).replace(/-/g, " ").trim();
        const elements = document.querySelectorAll("[aria-label]");
        for (const candidate of elements) {
          const label = candidate
            .getAttribute("aria-label")
            ?.toLowerCase()
            .replace(/[^a-z0-9]+/g, " ")
            .trim();
          if (label && label.startsWith(searchText.toLowerCase())) {
            return candidate;
          }
        }
      }

      if (elementId.startsWith("button-") || elementId.startsWith("a-")) {
        // Search by text content for buttons/links
        const [tag, ...textParts] = elementId.split("-");
        const searchText = textParts.join("-").replace(/-/g, " ").trim();
        const elements = document.querySelectorAll(tag);
        for (const candidate of elements) {
          const text = candidate.textContent
            ?.trim()
            .toLowerCase()
            .replace(/[^a-z0-9]+/g, " ")
            .trim();
          if (text && text.startsWith(searchText.toLowerCase())) {
            return candidate;
          }
        }
      }
    } catch (e) {
      console.warn("[Qontinui] Error finding element by ID:", e);
    }

    return null;
  }

  /**
   * Find the parent element with data-ui-id
   */
  function findParentUiId(el) {
    let parent = el.parentElement;
    while (parent) {
      if (parent.dataset?.uiId) {
        return parent.dataset.uiId;
      }
      parent = parent.parentElement;
    }
    return null;
  }

  /**
   * Get direct child UI IDs
   */
  function getChildUiIds(el) {
    return Array.from(el.querySelectorAll("[data-ui-id]"))
      .filter((child) => child.parentElement.closest("[data-ui-id]") === el)
      .map((child) => child.dataset.uiId);
  }

  /**
   * Get element label from various sources
   */
  function getElementLabel(el) {
    // Check aria-label
    if (el.getAttribute("aria-label")) {
      return el.getAttribute("aria-label");
    }
    // Check associated label
    if (el.id) {
      const label = document.querySelector(`label[for="${el.id}"]`);
      if (label) {
        return label.textContent?.trim();
      }
    }
    // Check placeholder
    if (el.placeholder) {
      return el.placeholder;
    }
    // Check title
    if (el.title) {
      return el.title;
    }
    // Use text content for buttons/links
    if (["button", "a"].includes(el.tagName.toLowerCase())) {
      return el.textContent?.trim().slice(0, 50);
    }
    return undefined;
  }

  /**
   * Infer element type from tag and attributes
   */
  function inferElementType(el) {
    const role = el.getAttribute("role");
    if (role) {
      switch (role) {
        case "button":
          return "button";
        case "textbox":
          return "input";
        case "checkbox":
          return "checkbox";
        case "radio":
          return "radio";
        case "link":
          return "link";
        case "listbox":
        case "combobox":
          return "select";
        case "menu":
          return "menu";
        case "menuitem":
          return "menuitem";
        case "tab":
          return "tab";
        case "dialog":
          return "dialog";
      }
    }

    const tag = el.tagName.toLowerCase();
    switch (tag) {
      case "button":
        return "button";
      case "input": {
        const type = el.type;
        if (type === "checkbox") return "checkbox";
        if (type === "radio") return "radio";
        if (type === "submit" || type === "button") return "button";
        return "input";
      }
      case "textarea":
        return "textarea";
      case "select":
        return "select";
      case "a":
        return "link";
      case "form":
        return "form";
      default:
        return "custom";
    }
  }

  /**
   * Infer available actions based on element type
   */
  function inferActions(type) {
    const baseActions = ["focus", "blur", "hover"];

    switch (type) {
      case "button":
        return [...baseActions, "click", "doubleClick", "rightClick"];
      case "input":
        return [...baseActions, "click", "type", "clear"];
      case "textarea":
        return [...baseActions, "click", "type", "clear"];
      case "select":
        return [...baseActions, "click", "select"];
      case "checkbox":
        return [...baseActions, "click", "check", "uncheck", "toggle"];
      case "radio":
        return [...baseActions, "click", "check"];
      case "link":
        return [...baseActions, "click"];
      default:
        return [...baseActions, "click"];
    }
  }

  /**
   * Check if element is visible
   */
  function isElementVisible(el) {
    const style = window.getComputedStyle(el);
    if (style.display === "none") return false;
    if (style.visibility === "hidden") return false;
    if (parseFloat(style.opacity) === 0) return false;

    const rect = el.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return false;

    // Check if in viewport
    return (
      rect.top < window.innerHeight &&
      rect.bottom > 0 &&
      rect.left < window.innerWidth &&
      rect.right > 0
    );
  }

  /**
   * Check if element is disabled
   */
  function isElementDisabled(el) {
    if ("disabled" in el && el.disabled) {
      return true;
    }
    if (el.getAttribute("aria-disabled") === "true") {
      return true;
    }
    return false;
  }

  /**
   * Build XPath for an element
   */
  function buildXPath(el) {
    if (!el || el.nodeType !== Node.ELEMENT_NODE) {
      return "";
    }

    // If element has an ID, use it
    if (el.id) {
      return `//*[@id="${el.id}"]`;
    }

    const parts = [];
    let current = el;

    while (current && current.nodeType === Node.ELEMENT_NODE) {
      let index = 1;
      let sibling = current.previousElementSibling;

      while (sibling) {
        if (sibling.tagName === current.tagName) {
          index++;
        }
        sibling = sibling.previousElementSibling;
      }

      const tagName = current.tagName.toLowerCase();
      const part = index > 1 ? `${tagName}[${index}]` : tagName;
      parts.unshift(part);

      if (current.parentElement) {
        current = current.parentElement;
      } else {
        break;
      }
    }

    return "/" + parts.join("/");
  }

  /**
   * Get computed visual styles for an element
   */
  function getComputedStyles(el) {
    try {
      const computed = window.getComputedStyle(el);
      return {
        color: computed.color,
        backgroundColor: computed.backgroundColor,
        fontSize: computed.fontSize,
        fontWeight: computed.fontWeight,
        fontFamily: computed.fontFamily,
        borderColor: computed.borderColor,
        borderWidth: computed.borderWidth,
        borderRadius: computed.borderRadius,
        padding: computed.padding,
        margin: computed.margin,
        display: computed.display,
        position: computed.position,
        opacity: computed.opacity,
        cursor: computed.cursor,
        textAlign: computed.textAlign,
        lineHeight: computed.lineHeight,
        zIndex: computed.zIndex,
      };
    } catch {
      return null;
    }
  }

  /**
   * Get extraction-relevant attributes for an element
   */
  function getExtractionAttributes(el) {
    return {
      // Selectors
      selector: buildUniqueSelector(el),
      xpath: buildXPath(el),

      // CSS classes
      classes: Array.from(el.classList),

      // HTML attributes useful for extraction
      href: el.href || el.getAttribute("href") || undefined,
      src: el.src || el.getAttribute("src") || undefined,
      alt: el.alt || el.getAttribute("alt") || undefined,
      title: el.title || el.getAttribute("title") || undefined,
      placeholder: el.placeholder || el.getAttribute("placeholder") || undefined,
      name: el.name || el.getAttribute("name") || undefined,
      inputType: el.type || undefined,

      // Form-related
      formAction: el.form?.action || undefined,
      formMethod: el.form?.method || undefined,

      // Data attributes (useful for extraction)
      dataAttributes: getDataAttributes(el),

      // Computed visual styles
      computedStyle: getComputedStyles(el),

      // Context information
      sourceUrl: window.location.href,
      viewportWidth: window.innerWidth,
      viewportHeight: window.innerHeight,

      // Additional metadata
      tagIndex: getTagIndex(el),
      depth: getElementDepth(el),

      // Fingerprint for cross-page element matching
      fingerprint: getElementFingerprint(el),
    };
  }

  /**
   * Get all data-* attributes from an element
   */
  function getDataAttributes(el) {
    const dataAttrs = {};
    for (const attr of el.attributes) {
      if (attr.name.startsWith("data-") && attr.name !== "data-ui-id") {
        dataAttrs[attr.name] = attr.value;
      }
    }
    return Object.keys(dataAttrs).length > 0 ? dataAttrs : undefined;
  }

  /**
   * Get the index of an element among siblings with same tag
   */
  function getTagIndex(el) {
    let index = 1;
    let sibling = el.previousElementSibling;
    while (sibling) {
      if (sibling.tagName === el.tagName) {
        index++;
      }
      sibling = sibling.previousElementSibling;
    }
    return index;
  }

  /**
   * Get the depth of an element in the DOM tree
   */
  function getElementDepth(el) {
    let depth = 0;
    let current = el;
    while (current.parentElement) {
      depth++;
      current = current.parentElement;
    }
    return depth;
  }

  // ============================================================================
  // Fingerprinting Functions
  // ============================================================================

  /**
   * Get the structural path of an element (tag names only, no IDs or classes).
   * This provides a stable identifier that works across page loads.
   * Example: "html > body > div > nav > ul > li > a"
   */
  function getStructuralPath(el) {
    const parts = [];
    let current = el;
    let maxDepth = 10; // Limit depth to avoid overly long paths

    while (current && current !== document.documentElement && maxDepth > 0) {
      const tagName = current.tagName.toLowerCase();

      // Add index among same-tag siblings for disambiguation
      const parent = current.parentElement;
      if (parent) {
        const sameTagSiblings = Array.from(parent.children).filter(
          (s) => s.tagName === current.tagName,
        );
        if (sameTagSiblings.length > 1) {
          const index = sameTagSiblings.indexOf(current) + 1;
          parts.unshift(`${tagName}[${index}]`);
        } else {
          parts.unshift(tagName);
        }
      } else {
        parts.unshift(tagName);
      }

      current = parent;
      maxDepth--;
    }

    return parts.join(" > ");
  }

  /**
   * Get the position zone of an element based on viewport position.
   * Zones: header, footer, sidebar-left, sidebar-right, main, modal, fixed-top, fixed-bottom
   */
  function getPositionZone(el) {
    const rect = el.getBoundingClientRect();
    const viewportWidth = window.innerWidth;
    const viewportHeight = window.innerHeight;
    const computed = window.getComputedStyle(el);

    // Check for fixed/sticky positioning (often headers/footers)
    const position = computed.position;
    if (position === "fixed" || position === "sticky") {
      if (rect.top < viewportHeight * 0.2) {
        return "fixed-top";
      } else if (rect.bottom > viewportHeight * 0.8) {
        return "fixed-bottom";
      }
    }

    // Check if element is in a modal/dialog
    const dialog = el.closest('dialog, [role="dialog"], [role="alertdialog"], [aria-modal="true"]');
    if (dialog) {
      return "modal";
    }

    // Calculate relative position
    const centerX = (rect.left + rect.right) / 2;
    const centerY = (rect.top + rect.bottom) / 2;

    const relativeX = centerX / viewportWidth;
    const relativeY = centerY / viewportHeight;

    // Determine zone based on position
    if (relativeY < 0.15) {
      return "header";
    } else if (relativeY > 0.85) {
      return "footer";
    } else if (relativeX < 0.2) {
      return "sidebar-left";
    } else if (relativeX > 0.8) {
      return "sidebar-right";
    } else {
      return "main";
    }
  }

  /**
   * Get the nearest ARIA landmark context for an element.
   * Returns the landmark role and optional label.
   */
  function getLandmarkContext(el) {
    const landmarkRoles = [
      "banner",
      "navigation",
      "main",
      "complementary",
      "contentinfo",
      "search",
      "form",
      "region",
    ];

    // Note: landmarkSelectors kept for potential future DOM-based lookup
    const _landmarkSelectors = [
      "header",
      "nav",
      "main",
      "aside",
      "footer",
      '[role="banner"]',
      '[role="navigation"]',
      '[role="main"]',
      '[role="complementary"]',
      '[role="contentinfo"]',
      '[role="search"]',
      '[role="form"]',
      '[role="region"]',
    ];

    // Find nearest landmark
    let current = el;
    while (current && current !== document.body) {
      // Check explicit role
      const role = current.getAttribute("role");
      if (role && landmarkRoles.includes(role)) {
        const label =
          current.getAttribute("aria-label") || current.getAttribute("aria-labelledby") || "";
        return { role, label: label.slice(0, 50) };
      }

      // Check implicit landmark (semantic elements)
      const tagName = current.tagName.toLowerCase();
      const implicitMap = {
        header: "banner",
        nav: "navigation",
        main: "main",
        aside: "complementary",
        footer: "contentinfo",
      };

      if (implicitMap[tagName]) {
        // header/footer are only landmarks if not inside article/aside/main/nav/section
        if (tagName === "header" || tagName === "footer") {
          const sectioning = current.closest("article, aside, main, nav, section");
          if (!sectioning || sectioning === current) {
            const label = current.getAttribute("aria-label") || "";
            return { role: implicitMap[tagName], label: label.slice(0, 50) };
          }
        } else {
          const label = current.getAttribute("aria-label") || "";
          return { role: implicitMap[tagName], label: label.slice(0, 50) };
        }
      }

      current = current.parentElement;
    }

    return { role: "none", label: "" };
  }

  /**
   * Classify element size into categories for fingerprinting.
   */
  function getSizeCategory(el) {
    const rect = el.getBoundingClientRect();
    const area = rect.width * rect.height;
    const viewportArea = window.innerWidth * window.innerHeight;
    const relativeArea = area / viewportArea;

    if (rect.width <= 32 && rect.height <= 32) {
      return "icon";
    } else if (rect.width <= 150 && rect.height <= 50) {
      return "button";
    } else if (relativeArea < 0.02) {
      return "small";
    } else if (relativeArea < 0.1) {
      return "medium";
    } else if (relativeArea < 0.3) {
      return "large";
    } else if (rect.width > window.innerWidth * 0.9) {
      return "fullwidth";
    } else {
      return "panel";
    }
  }

  /**
   * Normalize accessible name for fingerprinting.
   * Removes dynamic content patterns and normalizes text.
   */
  function normalizeAccessibleName(name) {
    if (!name) return "";

    let normalized = name
      .toLowerCase()
      .trim()
      // Remove common dynamic patterns
      .replace(/\d{1,2}:\d{2}(:\d{2})?(\s*[ap]m)?/gi, "[time]") // Times
      .replace(/\d{1,2}\/\d{1,2}\/\d{2,4}/g, "[date]") // Dates MM/DD/YYYY
      .replace(/\d{4}-\d{2}-\d{2}/g, "[date]") // Dates YYYY-MM-DD
      .replace(/\$[\d,.]+/g, "[price]") // Prices
      .replace(/€[\d,.]+/g, "[price]")
      .replace(/£[\d,.]+/g, "[price]")
      .replace(/\d+(\.\d+)?%/g, "[percent]") // Percentages
      .replace(/\b\d{5,}\b/g, "[number]") // Large numbers (IDs, etc.)
      .replace(/[a-f0-9]{8,}/gi, "[hash]") // Hex hashes
      .replace(/\s+/g, " ") // Normalize whitespace
      .slice(0, 100); // Limit length

    return normalized;
  }

  /**
   * Detect if element is part of a repeating pattern (list, grid, table).
   */
  function detectRepeatPattern(el) {
    const parent = el.parentElement;
    if (!parent) {
      return null;
    }

    // Check if parent is a list container
    const listContainers = ["ul", "ol", "menu", "datalist"];
    const parentTag = parent.tagName.toLowerCase();

    if (listContainers.includes(parentTag)) {
      const siblings = Array.from(parent.children).filter((s) => s.tagName === el.tagName);
      if (siblings.length > 1) {
        return {
          type: "list",
          containerTag: parentTag,
          index: siblings.indexOf(el),
          count: siblings.length,
          itemSelector: `${parentTag} > ${el.tagName.toLowerCase()}`,
        };
      }
    }

    // Check for ARIA list roles
    const parentRole = parent.getAttribute("role");
    if (["list", "listbox", "menu", "menubar", "tablist", "tree", "grid"].includes(parentRole)) {
      const itemRoles = [
        "listitem",
        "option",
        "menuitem",
        "menuitemcheckbox",
        "menuitemradio",
        "tab",
        "treeitem",
        "row",
        "gridcell",
      ];
      const elRole = el.getAttribute("role");
      if (elRole && itemRoles.includes(elRole)) {
        const siblings = Array.from(parent.children).filter(
          (s) => s.getAttribute("role") === elRole,
        );
        return {
          type: parentRole,
          containerRole: parentRole,
          index: siblings.indexOf(el),
          count: siblings.length,
          itemRole: elRole,
        };
      }
    }

    // Check for table structure
    if (el.tagName.toLowerCase() === "tr") {
      const table = el.closest('table, [role="grid"], [role="table"]');
      if (table) {
        const rows = table.querySelectorAll('tr, [role="row"]');
        return {
          type: "table-row",
          index: Array.from(rows).indexOf(el),
          count: rows.length,
        };
      }
    }

    // Check for grid-like layout (many similar siblings)
    const similarSiblings = Array.from(parent.children).filter((s) => {
      if (s === el) return true;
      // Check if sibling has same tag and similar classes
      if (s.tagName !== el.tagName) return false;
      const elClasses = Array.from(el.classList);
      const sibClasses = Array.from(s.classList);
      // At least 50% class overlap
      const overlap = elClasses.filter((c) => sibClasses.includes(c)).length;
      return overlap >= Math.min(elClasses.length, sibClasses.length) * 0.5;
    });

    if (similarSiblings.length >= 3) {
      return {
        type: "grid-item",
        index: similarSiblings.indexOf(el),
        count: similarSiblings.length,
      };
    }

    return null;
  }

  /**
   * Simple hash function for fingerprint (djb2 algorithm).
   */
  function simpleHash(str) {
    let hash = 5381;
    for (let i = 0; i < str.length; i++) {
      hash = (hash << 5) + hash + str.charCodeAt(i);
      hash = hash & hash; // Convert to 32-bit integer
    }
    return Math.abs(hash).toString(36);
  }

  /**
   * Generate a complete fingerprint for an element.
   * This fingerprint can be used to match the same element across different page captures.
   */
  function getElementFingerprint(el) {
    const rect = el.getBoundingClientRect();
    const role = el.getAttribute("role") || getImplicitRole(el) || el.tagName.toLowerCase();
    const accessibleName = computeAccessibleName(el) || "";
    const structuralPath = getStructuralPath(el);
    const positionZone = getPositionZone(el);
    const landmarkContext = getLandmarkContext(el);
    const sizeCategory = getSizeCategory(el);
    const repeatPattern = detectRepeatPattern(el);

    // Calculate relative position (0-1 percentage of viewport)
    const relativePosition = {
      top: Math.round((rect.top / window.innerHeight) * 100) / 100,
      left: Math.round((rect.left / window.innerWidth) * 100) / 100,
    };

    // Normalize accessible name for matching
    const normalizedName = normalizeAccessibleName(accessibleName);

    // Build the fingerprint object
    const fingerprint = {
      // Structural identity (most stable)
      structuralPath,
      positionZone,
      landmarkContext: landmarkContext.role,
      landmarkLabel: landmarkContext.label || undefined,

      // Semantic identity
      role,
      tagName: el.tagName.toLowerCase(),
      accessibleName: normalizedName || undefined,

      // Visual identity
      sizeCategory,
      relativePosition,

      // Repeat pattern (for list/grid items)
      isRepeating: repeatPattern !== null,
      repeatPattern: repeatPattern || undefined,

      // Hash for quick matching (computed from stable properties)
      hash: null, // Will be computed below
    };

    // Compute hash from stable properties
    const hashInput = [
      fingerprint.structuralPath,
      fingerprint.positionZone,
      fingerprint.landmarkContext,
      fingerprint.role,
      fingerprint.tagName,
      fingerprint.accessibleName || "",
      fingerprint.sizeCategory,
      // Include relative position with some tolerance (round to 0.1)
      Math.round(relativePosition.top * 10),
      Math.round(relativePosition.left * 10),
    ].join("|");

    fingerprint.hash = simpleHash(hashInput);

    return fingerprint;
  }

  // ============================================================================
  // Accessibility Functions
  // ============================================================================

  /**
   * Map of HTML elements to their implicit ARIA roles
   * Based on WAI-ARIA 1.2 specification
   */
  const IMPLICIT_ROLE_MAP = {
    a: (el) => (el.hasAttribute("href") ? "link" : null),
    article: () => "article",
    aside: () => "complementary",
    button: () => "button",
    datalist: () => "listbox",
    details: () => "group",
    dialog: () => "dialog",
    fieldset: () => "group",
    figure: () => "figure",
    footer: (el) => (isLandmark(el) ? "contentinfo" : null),
    form: (el) =>
      el.hasAttribute("aria-label") || el.hasAttribute("aria-labelledby") || el.hasAttribute("name")
        ? "form"
        : null,
    h1: () => "heading",
    h2: () => "heading",
    h3: () => "heading",
    h4: () => "heading",
    h5: () => "heading",
    h6: () => "heading",
    header: (el) => (isLandmark(el) ? "banner" : null),
    hr: () => "separator",
    img: (el) => (el.getAttribute("alt") === "" ? "presentation" : "img"),
    input: (el) => getInputRole(el),
    li: () => "listitem",
    main: () => "main",
    menu: () => "list",
    meter: () => "meter",
    nav: () => "navigation",
    ol: () => "list",
    optgroup: () => "group",
    option: () => "option",
    output: () => "status",
    progress: () => "progressbar",
    section: (el) =>
      el.hasAttribute("aria-label") || el.hasAttribute("aria-labelledby") ? "region" : null,
    select: (el) => (el.hasAttribute("multiple") || el.size > 1 ? "listbox" : "combobox"),
    summary: () => "button",
    table: () => "table",
    tbody: () => "rowgroup",
    td: () => "cell",
    textarea: () => "textbox",
    tfoot: () => "rowgroup",
    th: (el) => (el.getAttribute("scope") === "col" ? "columnheader" : "rowheader"),
    thead: () => "rowgroup",
    tr: () => "row",
    ul: () => "list",
  };

  /**
   * Check if element is a landmark (not inside article/aside/main/nav/section)
   */
  function isLandmark(el) {
    const sectioning = el.closest("article, aside, main, nav, section");
    return !sectioning || sectioning === el;
  }

  /**
   * Get implicit role for input elements based on type
   */
  function getInputRole(el) {
    const type = (el.type || "text").toLowerCase();
    switch (type) {
      case "button":
      case "image":
      case "reset":
      case "submit":
        return "button";
      case "checkbox":
        return "checkbox";
      case "email":
      case "tel":
      case "text":
      case "url":
        return el.hasAttribute("list") ? "combobox" : "textbox";
      case "number":
        return "spinbutton";
      case "radio":
        return "radio";
      case "range":
        return "slider";
      case "search":
        return el.hasAttribute("list") ? "combobox" : "searchbox";
      default:
        return "textbox";
    }
  }

  /**
   * Get the implicit ARIA role for an element
   */
  function getImplicitRole(el) {
    const tagName = el.tagName.toLowerCase();
    const roleGetter = IMPLICIT_ROLE_MAP[tagName];
    if (roleGetter) {
      return roleGetter(el) || null;
    }
    return null;
  }

  /**
   * Compute the accessible name for an element
   * Follows a simplified version of the ARIA name computation algorithm
   */
  function computeAccessibleName(el) {
    // Step 1: aria-labelledby (highest priority)
    const labelledBy = el.getAttribute("aria-labelledby");
    if (labelledBy) {
      const names = labelledBy
        .split(/\s+/)
        .map((id) => {
          const labelEl = document.getElementById(id);
          return labelEl ? labelEl.textContent?.trim() : null;
        })
        .filter(Boolean);
      if (names.length > 0) {
        return names.join(" ");
      }
    }

    // Step 2: aria-label
    const ariaLabel = el.getAttribute("aria-label");
    if (ariaLabel) {
      return ariaLabel;
    }

    // Step 3: Native label association (for form elements)
    if (
      el instanceof HTMLInputElement ||
      el instanceof HTMLSelectElement ||
      el instanceof HTMLTextAreaElement
    ) {
      // Check for <label for="id">
      if (el.id) {
        const label = document.querySelector(`label[for="${CSS.escape(el.id)}"]`);
        if (label) {
          return label.textContent?.trim() || null;
        }
      }
      // Check for wrapping <label>
      const parentLabel = el.closest("label");
      if (parentLabel) {
        // Get text excluding the input itself
        const clone = parentLabel.cloneNode(true);
        const inputs = clone.querySelectorAll("input, select, textarea");
        inputs.forEach((input) => input.remove());
        const text = clone.textContent?.trim();
        if (text) return text;
      }
    }

    // Step 4: alt attribute (for images)
    if (el instanceof HTMLImageElement && el.alt) {
      return el.alt;
    }

    // Step 5: title attribute
    const title = el.getAttribute("title");
    if (title) {
      return title;
    }

    // Step 6: placeholder (for inputs)
    if ((el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) && el.placeholder) {
      return el.placeholder;
    }

    // Step 7: Text content (for buttons, links, etc.)
    const role = el.getAttribute("role") || getImplicitRole(el);
    if (["button", "link", "menuitem", "tab", "option"].includes(role)) {
      return el.textContent?.trim() || null;
    }

    return null;
  }

  /**
   * Compute the accessible description for an element
   */
  function computeAccessibleDescription(el) {
    // aria-describedby
    const describedBy = el.getAttribute("aria-describedby");
    if (describedBy) {
      const descriptions = describedBy
        .split(/\s+/)
        .map((id) => {
          const descEl = document.getElementById(id);
          return descEl ? descEl.textContent?.trim() : null;
        })
        .filter(Boolean);
      if (descriptions.length > 0) {
        return descriptions.join(" ");
      }
    }

    // title attribute (if not used for accessible name)
    const title = el.getAttribute("title");
    const accessibleName = computeAccessibleName(el);
    if (title && title !== accessibleName) {
      return title;
    }

    return null;
  }

  /**
   * Check if an element is naturally focusable (without tabindex)
   */
  function isNaturallyFocusable(el) {
    const tag = el.tagName.toLowerCase();

    // Links with href
    if (tag === "a" && el.hasAttribute("href")) return true;

    // Buttons
    if (tag === "button") return true;

    // Form elements (unless disabled)
    if (["input", "select", "textarea"].includes(tag) && !el.disabled) return true;

    // Summary elements
    if (tag === "summary") return true;

    // Elements with contenteditable
    if (el.isContentEditable) return true;

    return false;
  }

  /**
   * Get the element's tabindex value
   */
  function getTabIndex(el) {
    const tabindexAttr = el.getAttribute("tabindex");
    if (tabindexAttr !== null) {
      const parsed = parseInt(tabindexAttr, 10);
      return isNaN(parsed) ? -1 : parsed;
    }
    // For naturally focusable elements without explicit tabindex, return 0
    return isNaturallyFocusable(el) ? 0 : -1;
  }

  /**
   * Check if element is in the tab order
   */
  function isInTabOrder(el) {
    const tabIndex = getTabIndex(el);
    if (tabIndex < 0) return false;
    if (isElementDisabled(el)) return false;
    if (!isElementVisible(el)) return false;
    return true;
  }

  /**
   * Check if element is keyboard accessible
   */
  function isKeyboardAccessible(el) {
    // Element must be focusable
    const tabIndex = getTabIndex(el);
    if (tabIndex < -1) return false;

    // Check if disabled
    if (isElementDisabled(el)) return false;

    // Check if hidden from accessibility tree
    if (el.getAttribute("aria-hidden") === "true") return false;

    // Check visibility
    if (!isElementVisible(el)) return false;

    return tabIndex >= 0 || isNaturallyFocusable(el);
  }

  /**
   * Parse aria-checked attribute value
   */
  function parseAriaChecked(value) {
    if (value === "true") return true;
    if (value === "false") return false;
    if (value === "mixed") return "mixed";
    return undefined;
  }

  /**
   * Parse aria-pressed attribute value
   */
  function parseAriaPressed(value) {
    if (value === "true") return true;
    if (value === "false") return false;
    if (value === "mixed") return "mixed";
    return undefined;
  }

  /**
   * Get comprehensive accessibility information for an element
   */
  function getElementAccessibility(el) {
    const explicitRole = el.getAttribute("role");
    const implicitRole = getImplicitRole(el);
    const tabIndex = getTabIndex(el);

    return {
      role: explicitRole || implicitRole || "generic",
      accessibleName: computeAccessibleName(el) || undefined,
      accessibleDescription: computeAccessibleDescription(el) || undefined,
      ariaLabel: el.getAttribute("aria-label") || undefined,
      ariaLabelledBy: el.getAttribute("aria-labelledby") || undefined,
      ariaDescribedBy: el.getAttribute("aria-describedby") || undefined,
      ariaExpanded:
        el.getAttribute("aria-expanded") === "true"
          ? true
          : el.getAttribute("aria-expanded") === "false"
            ? false
            : undefined,
      ariaSelected:
        el.getAttribute("aria-selected") === "true"
          ? true
          : el.getAttribute("aria-selected") === "false"
            ? false
            : undefined,
      ariaChecked: parseAriaChecked(el.getAttribute("aria-checked")),
      ariaPressed: parseAriaPressed(el.getAttribute("aria-pressed")),
      ariaReadOnly:
        el.getAttribute("aria-readonly") === "true"
          ? true
          : el.getAttribute("aria-readonly") === "false"
            ? false
            : undefined,
      ariaHidden: el.getAttribute("aria-hidden") === "true" ? true : undefined,
      ariaDisabled: el.getAttribute("aria-disabled") === "true" ? true : undefined,
      ariaRequired: el.getAttribute("aria-required") === "true" ? true : undefined,
      ariaLive: ["off", "polite", "assertive"].includes(el.getAttribute("aria-live"))
        ? el.getAttribute("aria-live")
        : undefined,
      tabIndex: tabIndex,
      isInTabOrder: isInTabOrder(el),
      isKeyboardAccessible: isKeyboardAccessible(el),
      implicitRole: implicitRole || undefined,
      hasExplicitRole: !!explicitRole,
    };
  }

  /**
   * Create highlight overlay for an element
   */
  function createOverlay(el, color = OVERLAY_COLOR, showLabel = true) {
    const overlay = document.createElement("div");
    overlay.className = "__qontinui-overlay__";
    const rect = el.getBoundingClientRect();

    Object.assign(overlay.style, {
      position: "fixed",
      top: rect.top + "px",
      left: rect.left + "px",
      width: rect.width + "px",
      height: rect.height + "px",
      backgroundColor: color,
      border: `2px solid ${OVERLAY_BORDER}`,
      borderRadius: "2px",
      pointerEvents: "none",
      zIndex: "999999",
      transition: "all 0.1s ease-out",
    });

    // Add label if element has data-ui-id
    if (showLabel && el.dataset?.uiId) {
      const label = document.createElement("div");
      label.textContent = el.dataset.uiId;
      Object.assign(label.style, {
        position: "absolute",
        top: "-20px",
        left: "0",
        backgroundColor: LABEL_BG,
        color: "white",
        fontSize: "11px",
        fontFamily: "-apple-system, BlinkMacSystemFont, sans-serif",
        padding: "2px 6px",
        borderRadius: "2px",
        whiteSpace: "nowrap",
        maxWidth: "200px",
        overflow: "hidden",
        textOverflow: "ellipsis",
      });
      overlay.appendChild(label);
    }

    document.body.appendChild(overlay);
    return overlay;
  }

  /**
   * Clear hover overlay
   */
  function clearHoverOverlay() {
    if (hoverOverlay) {
      hoverOverlay.remove();
      hoverOverlay = null;
    }
  }

  /**
   * Enable element picker mode
   */
  function enablePicker() {
    pickerEnabled = true;
    document.body.style.cursor = "crosshair";

    document.addEventListener("mouseover", onPickerMouseOver, true);
    document.addEventListener("mouseout", onPickerMouseOut, true);
    document.addEventListener("click", onPickerClick, true);
    document.addEventListener("keydown", onPickerKeyDown, true);
  }

  /**
   * Disable element picker mode
   */
  function disablePicker() {
    pickerEnabled = false;
    document.body.style.cursor = "";

    document.removeEventListener("mouseover", onPickerMouseOver, true);
    document.removeEventListener("mouseout", onPickerMouseOut, true);
    document.removeEventListener("click", onPickerClick, true);
    document.removeEventListener("keydown", onPickerKeyDown, true);

    clearHoverOverlay();
  }

  function onPickerMouseOver(e) {
    if (!pickerEnabled) return;

    // Find closest UI Bridge element or use target
    const target = e.target.closest("[data-ui-id]") || e.target;
    if (target === hoveredElement) return;

    hoveredElement = target;
    clearHoverOverlay();
    hoverOverlay = createOverlay(target, HOVER_COLOR, true);
  }

  function onPickerMouseOut(_e) {
    // Keep overlay until we hover over something else
  }

  function onPickerClick(e) {
    if (!pickerEnabled) return;

    e.preventDefault();
    e.stopPropagation();

    const target = e.target.closest("[data-ui-id]") || e.target;
    _selectedElement = target;

    // Build element data
    const rect = target.getBoundingClientRect();
    const elementData = {
      id: target.dataset?.uiId || null,
      tagName: target.tagName.toLowerCase(),
      type: inferElementType(target),
      bounds: {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
      },
      hasUiId: !!target.dataset?.uiId,
      visible: isElementVisible(target),
      enabled: !isElementDisabled(target),
      value: "value" in target ? target.value : undefined,
      text: target.textContent?.trim().slice(0, 100),
      label: getElementLabel(target),
      actions: inferActions(inferElementType(target)),
    };

    // Send selection to bridge
    window.postMessage(
      {
        type: "__QONTINUI_ELEMENT_SELECTED__",
        data: elementData,
      },
      "*",
    );

    disablePicker();
  }

  function onPickerKeyDown(e) {
    if (e.key === "Escape") {
      disablePicker();
      window.postMessage(
        {
          type: "__QONTINUI_PICKER_CANCELLED__",
        },
        "*",
      );
    }
  }

  /**
   * Show overlays for all registered elements
   */
  function showOverlays() {
    hideOverlays();
    _overlayEnabled = true;

    const elements = document.querySelectorAll("[data-ui-id]");
    elements.forEach((el) => {
      const overlay = createOverlay(el, OVERLAY_COLOR, true);
      overlayElements.set(el.dataset.uiId, overlay);
    });
  }

  /**
   * Hide all overlays
   */
  function hideOverlays() {
    _overlayEnabled = false;
    overlayElements.forEach((overlay) => overlay.remove());
    overlayElements.clear();
  }

  /**
   * Highlight a specific element by ID
   */
  function highlightElement(elementId) {
    const el = findElementById(elementId);
    if (!el) {
      return { success: false, error: `Element not found: ${elementId}` };
    }

    // Remove existing highlight
    const existingHighlight = document.querySelector(".__qontinui-highlight__");
    if (existingHighlight) {
      existingHighlight.remove();
    }

    // Create highlight
    const highlight = createOverlay(el, "rgba(34, 197, 94, 0.3)", true);
    highlight.className = "__qontinui-highlight__";
    highlight.style.border = "2px solid rgba(34, 197, 94, 0.8)";

    // Auto-remove after 2 seconds
    setTimeout(() => {
      highlight.remove();
    }, 2000);

    // Scroll into view
    el.scrollIntoView({ behavior: "smooth", block: "center" });

    return { success: true };
  }

  /**
   * Execute action on element
   */
  function executeAction(elementId, action, params = {}) {
    // Use the new findElementById that works with both UI Bridge and discovered elements
    const el = findElementById(elementId);
    if (!el) {
      return { success: false, error: `Element not found: ${elementId}` };
    }

    try {
      switch (action) {
        case "click":
          // Use proper mouse events for better compatibility with React/Radix UI components
          // Simple el.click() doesn't always trigger React's synthetic event handlers
          {
            const rect = el.getBoundingClientRect();
            const centerX = rect.left + rect.width / 2;
            const centerY = rect.top + rect.height / 2;

            // Focus the element first (important for keyboard-accessible components)
            if (el.focus) el.focus();

            // Dispatch mousedown, mouseup, then click (simulates real user interaction)
            const mouseEventInit = {
              bubbles: true,
              cancelable: true,
              view: window,
              clientX: centerX,
              clientY: centerY,
              screenX: centerX,
              screenY: centerY,
              button: 0,
              buttons: 1,
            };

            el.dispatchEvent(new MouseEvent("mousedown", mouseEventInit));
            el.dispatchEvent(new MouseEvent("mouseup", mouseEventInit));
            el.dispatchEvent(new MouseEvent("click", mouseEventInit));

            // Also try el.click() as a fallback for elements that expect it
            try {
              el.click();
            } catch {
              /* ignore */
            }
          }
          break;
        case "doubleClick":
          el.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
          break;
        case "rightClick":
          el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
          break;
        case "focus":
          el.focus();
          break;
        case "blur":
          el.blur();
          break;
        case "hover":
          el.dispatchEvent(new MouseEvent("mouseenter", { bubbles: true }));
          el.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
          break;
        case "clear":
          if ("value" in el) {
            el.value = "";
            el.dispatchEvent(new Event("input", { bubbles: true }));
            el.dispatchEvent(new Event("change", { bubbles: true }));
          }
          break;
        case "type":
        case "fill": // Alias for 'type'
          if ("value" in el) {
            el.focus();
            if (params.clear) {
              el.value = "";
            }
            el.value = params.text || "";
            el.dispatchEvent(new Event("input", { bubbles: true }));
            el.dispatchEvent(new Event("change", { bubbles: true }));
          }
          break;
        case "select":
          if (el.tagName === "SELECT") {
            el.value = params.value;
            el.dispatchEvent(new Event("change", { bubbles: true }));
          }
          break;
        case "check":
          if ("checked" in el && !el.checked) {
            el.click();
          }
          break;
        case "uncheck":
          if ("checked" in el && el.checked) {
            el.click();
          }
          break;
        case "toggle":
          if ("checked" in el) {
            el.click();
          }
          break;
        default:
          return { success: false, error: `Unknown action: ${action}` };
      }

      // Return updated state
      const rect = el.getBoundingClientRect();
      return {
        success: true,
        elementState: {
          visible: isElementVisible(el),
          enabled: !isElementDisabled(el),
          focused: document.activeElement === el,
          value: "value" in el ? el.value : undefined,
          checked: "checked" in el ? el.checked : undefined,
          bounds: {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
          },
        },
      };
    } catch (err) {
      return { success: false, error: err.message };
    }
  }

  /**
   * Get element state by ID
   */
  function getElementState(elementId) {
    const el = findElementById(elementId);
    if (!el) {
      return { success: false, error: `Element not found: ${elementId}` };
    }

    const rect = el.getBoundingClientRect();
    return {
      success: true,
      state: {
        id: elementId,
        visible: isElementVisible(el),
        enabled: !isElementDisabled(el),
        focused: document.activeElement === el,
        value: "value" in el ? el.value : undefined,
        checked: "checked" in el ? el.checked : undefined,
        text: el.textContent?.trim().slice(0, 100),
        bounds: {
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
          top: rect.top,
          right: rect.right,
          bottom: rect.bottom,
          left: rect.left,
        },
      },
    };
  }

  /**
   * Clear highlight
   */
  function clearHighlight() {
    const existingHighlight = document.querySelector(".__qontinui-highlight__");
    if (existingHighlight) {
      existingHighlight.remove();
    }
    return { success: true };
  }

  /**
   * Get state snapshot (for state management)
   * Returns info about UI Bridge state if available, or basic info otherwise
   */
  function getStateSnapshot() {
    // Try to access UI Bridge state if it's exposed
    const uiBridge = window.__UI_BRIDGE__;
    if (uiBridge && typeof uiBridge.getStateSnapshot === "function") {
      return { success: true, ...uiBridge.getStateSnapshot() };
    }

    // Return basic info about registered elements
    const elements = getRegisteredElements();
    return {
      success: true,
      timestamp: Date.now(),
      activeStates: [],
      states: [],
      transitions: [],
      elementCount: elements.length,
    };
  }

  // ==========================================================================
  // Visual Analysis - Color Utilities
  // ==========================================================================

  /**
   * Parse a CSS color string (rgb, rgba, hex) into {r, g, b, a} components.
   * Returns null if parsing fails.
   */
  function parseColor(colorString) {
    if (!colorString || colorString === "transparent") return { r: 0, g: 0, b: 0, a: 0 };
    // rgb(r, g, b) or rgba(r, g, b, a)
    const rgbMatch = colorString.match(
      /rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)\s*(?:,\s*([\d.]+))?\s*\)/,
    );
    if (rgbMatch) {
      return {
        r: parseInt(rgbMatch[1], 10),
        g: parseInt(rgbMatch[2], 10),
        b: parseInt(rgbMatch[3], 10),
        a: rgbMatch[4] !== undefined ? parseFloat(rgbMatch[4]) : 1,
      };
    }
    // #rrggbb or #rgb
    const hexMatch = colorString.match(/^#([0-9a-f]{3,8})$/i);
    if (hexMatch) {
      const hex = hexMatch[1];
      if (hex.length === 3) {
        return {
          r: parseInt(hex[0] + hex[0], 16),
          g: parseInt(hex[1] + hex[1], 16),
          b: parseInt(hex[2] + hex[2], 16),
          a: 1,
        };
      }
      if (hex.length >= 6) {
        return {
          r: parseInt(hex.slice(0, 2), 16),
          g: parseInt(hex.slice(2, 4), 16),
          b: parseInt(hex.slice(4, 6), 16),
          a: hex.length === 8 ? parseInt(hex.slice(6, 8), 16) / 255 : 1,
        };
      }
    }
    return null;
  }

  /**
   * Blend a semi-transparent foreground color over an opaque background.
   */
  function blendColors(fg, bg) {
    const a = fg.a;
    return {
      r: Math.round(fg.r * a + bg.r * (1 - a)),
      g: Math.round(fg.g * a + bg.g * (1 - a)),
      b: Math.round(fg.b * a + bg.b * (1 - a)),
      a: 1,
    };
  }

  /**
   * Walk up the DOM tree to find the effective background color behind an element.
   * Blends semi-transparent backgrounds along the way.
   */
  function getEffectiveBackgroundColor(el) {
    let current = el;
    const layers = [];
    while (current && current !== document.documentElement) {
      try {
        const bg = window.getComputedStyle(current).backgroundColor;
        const parsed = parseColor(bg);
        if (parsed && parsed.a > 0) {
          layers.unshift(parsed);
          if (parsed.a === 1) break; // opaque - stop here
        }
      } catch {
        break;
      }
      current = current.parentElement;
    }
    // Start with white (page default)
    let result = { r: 255, g: 255, b: 255, a: 1 };
    for (const layer of layers) {
      result = blendColors(layer, result);
    }
    return result;
  }

  /**
   * Calculate WCAG 2.1 relative luminance.
   */
  function relativeLuminance(r, g, b) {
    const [rs, gs, bs] = [r, g, b].map((c) => {
      c = c / 255;
      return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
    });
    return 0.2126 * rs + 0.7152 * gs + 0.0722 * bs;
  }

  /**
   * Calculate WCAG contrast ratio between two colors.
   */
  function contrastRatio(c1, c2) {
    const L1 = relativeLuminance(c1.r, c1.g, c1.b);
    const L2 = relativeLuminance(c2.r, c2.g, c2.b);
    const lighter = Math.max(L1, L2);
    const darker = Math.min(L1, L2);
    return (lighter + 0.05) / (darker + 0.05);
  }

  /**
   * Check if two colors are approximately equal within a tolerance.
   */
  function colorsMatch(c1, c2, tolerance) {
    tolerance = tolerance || 5;
    return (
      Math.abs(c1.r - c2.r) <= tolerance &&
      Math.abs(c1.g - c2.g) <= tolerance &&
      Math.abs(c1.b - c2.b) <= tolerance
    );
  }

  /**
   * Convert a parsed color to a CSS string for reporting.
   */
  function colorToString(c) {
    if (!c) return "unknown";
    if (c.a < 1) return `rgba(${c.r}, ${c.g}, ${c.b}, ${c.a.toFixed(2)})`;
    return `rgb(${c.r}, ${c.g}, ${c.b})`;
  }

  // ==========================================================================
  // Visual Analysis - Collect Visible Elements
  // ==========================================================================

  /**
   * Collect all visible elements with their bounds and computed styles in a
   * single reflow pass, to avoid layout thrashing during analysis.
   */
  function collectVisibleElementData(scope) {
    const root = scope ? document.querySelector(scope) : document.body;
    if (!root) return [];

    const all = root.querySelectorAll("*");
    const data = [];
    for (let i = 0; i < all.length; i++) {
      const el = all[i];
      // Skip invisible and script/style/meta elements
      const tag = el.tagName.toLowerCase();
      if (
        tag === "script" ||
        tag === "style" ||
        tag === "meta" ||
        tag === "link" ||
        tag === "noscript" ||
        tag === "template"
      )
        continue;

      const style = window.getComputedStyle(el);
      if (style.display === "none") continue;
      if (style.visibility === "hidden") continue;

      const rect = el.getBoundingClientRect();
      if (rect.width === 0 && rect.height === 0) continue;

      data.push({
        el,
        tag,
        rect: {
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
          top: rect.top,
          right: rect.right,
          bottom: rect.bottom,
          left: rect.left,
        },
        style: {
          color: style.color,
          backgroundColor: style.backgroundColor,
          fontSize: style.fontSize,
          fontWeight: style.fontWeight,
          fontFamily: style.fontFamily,
          display: style.display,
          position: style.position,
          opacity: style.opacity,
          zIndex: style.zIndex,
          overflow: style.overflow,
          overflowX: style.overflowX,
          overflowY: style.overflowY,
          borderRadius: style.borderRadius,
          boxShadow: style.boxShadow,
          borderWidth: style.borderWidth,
          pointerEvents: style.pointerEvents,
          flexDirection: style.flexDirection,
          gap: style.gap,
          textAlign: style.textAlign,
          lineHeight: style.lineHeight,
        },
        selector: buildUniqueSelector(el),
        text: (el.textContent || "").trim().slice(0, 100),
        isInteractive: el.matches(
          'a[href], button, input, select, textarea, [role="button"], [role="link"], [tabindex]',
        ),
        parent: el.parentElement,
        children: Array.from(el.children),
      });
    }
    return data;
  }

  // ==========================================================================
  // Visual Analysis - Layout Analysis
  // ==========================================================================

  function analyzeLayout(params) {
    const startTime = performance.now();
    const tolerance = params?.tolerance || 2;
    const maxElements = params?.maxElements || 500;
    const checks = params?.checks || [
      "overlap",
      "alignment",
      "spacing",
      "overflow",
      "stacking",
      "viewport",
    ];
    const findings = [];
    const elements = collectVisibleElementData(params?.scope);
    const subset = elements.slice(0, maxElements);

    // Build parent-children map
    const childrenMap = new Map();
    for (const item of subset) {
      if (!item.parent) continue;
      if (!childrenMap.has(item.parent)) childrenMap.set(item.parent, []);
      childrenMap.get(item.parent).push(item);
    }

    // --- Overlap Detection ---
    if (checks.includes("overlap")) {
      for (const [, siblings] of childrenMap) {
        if (siblings.length < 2) continue;
        for (let i = 0; i < siblings.length; i++) {
          for (let j = i + 1; j < siblings.length; j++) {
            const a = siblings[i],
              b = siblings[j];
            // Skip if either is positioned (intentional overlay)
            if (
              a.style.position === "absolute" ||
              a.style.position === "fixed" ||
              b.style.position === "absolute" ||
              b.style.position === "fixed"
            )
              continue;
            if (a.style.pointerEvents === "none" || b.style.pointerEvents === "none") continue;

            const overlapX = Math.max(
              0,
              Math.min(a.rect.right, b.rect.right) - Math.max(a.rect.left, b.rect.left),
            );
            const overlapY = Math.max(
              0,
              Math.min(a.rect.bottom, b.rect.bottom) - Math.max(a.rect.top, b.rect.top),
            );

            if (overlapX > tolerance && overlapY > tolerance) {
              findings.push({
                type: "overlap",
                severity: a.isInteractive || b.isInteractive ? "error" : "warning",
                category: "layout",
                elementA: { selector: a.selector, bounds: a.rect },
                elementB: { selector: b.selector, bounds: b.rect },
                overlapArea: Math.round(overlapX * overlapY),
                context: `Siblings overlap by ${Math.round(overlapX)}x${Math.round(overlapY)}px`,
              });
            }
          }
        }
      }
    }

    // --- Alignment Analysis ---
    if (checks.includes("alignment")) {
      for (const [, siblings] of childrenMap) {
        if (siblings.length < 3) continue;
        // Check left-edge alignment
        const lefts = siblings.map((s) => Math.round(s.rect.left));
        const leftMode = mostCommon(lefts);
        for (const sib of siblings) {
          const dev = Math.abs(Math.round(sib.rect.left) - leftMode);
          if (dev > tolerance && dev < 50) {
            findings.push({
              type: "alignment",
              severity: "warning",
              category: "layout",
              element: { selector: sib.selector, leftEdge: Math.round(sib.rect.left) },
              expectedEdge: leftMode,
              deviation: dev,
              context: `Left edge ${Math.round(sib.rect.left)}px deviates from group mode ${leftMode}px`,
            });
          }
        }
      }
    }

    // --- Spacing Consistency ---
    if (checks.includes("spacing")) {
      for (const [, siblings] of childrenMap) {
        if (siblings.length < 3) continue;
        // Sort by position (top for vertical, left for horizontal)
        const sorted = [...siblings].sort((a, b) => a.rect.top - b.rect.top);
        const isVertical =
          Math.abs(sorted[0].rect.top - sorted[sorted.length - 1].rect.top) >
          Math.abs(sorted[0].rect.left - sorted[sorted.length - 1].rect.left);
        const gaps = [];
        for (let i = 1; i < sorted.length; i++) {
          const gap = isVertical
            ? sorted[i].rect.top - sorted[i - 1].rect.bottom
            : sorted[i].rect.left - sorted[i - 1].rect.right;
          gaps.push(Math.round(gap));
        }
        if (gaps.length < 2) continue;
        const mean = gaps.reduce((a, b) => a + b, 0) / gaps.length;
        if (mean <= 0) continue; // overlapping or zero-gap — skip
        for (let i = 0; i < gaps.length; i++) {
          if (Math.abs(gaps[i] - mean) > Math.max(tolerance, mean * 0.4)) {
            findings.push({
              type: "spacingInconsistency",
              severity: "warning",
              category: "layout",
              container: { selector: buildUniqueSelector(sorted[0].parent) },
              gaps,
              meanGap: Math.round(mean),
              outlierIndex: i,
              outlierGap: gaps[i],
              context: `Gap ${gaps[i]}px at index ${i} deviates from mean ${Math.round(mean)}px`,
            });
            break; // one finding per container
          }
        }
      }
    }

    // --- Overflow Detection ---
    if (checks.includes("overflow")) {
      for (const item of subset) {
        if (item.children.length === 0) continue;
        const pOverflow = item.style.overflow;
        // Only flag if overflow is visible (default) — hidden/scroll/auto handle it
        if (pOverflow !== "visible" && pOverflow !== "") continue;
        const pRect = item.rect;
        // Only check containers with explicit sizing
        if (pRect.width === 0 || pRect.height === 0) continue;

        for (const child of item.children) {
          const childData = subset.find((d) => d.el === child);
          if (!childData) continue;
          const cRect = childData.rect;
          const rightOverflow = cRect.right - pRect.right;
          const bottomOverflow = cRect.bottom - pRect.bottom;

          if (rightOverflow > tolerance) {
            findings.push({
              type: "overflow",
              severity: "warning",
              category: "layout",
              parent: { selector: item.selector, overflow: pOverflow },
              child: { selector: childData.selector },
              direction: "right",
              amount: Math.round(rightOverflow),
              context: `Child extends ${Math.round(rightOverflow)}px beyond parent right edge`,
            });
          }
          if (bottomOverflow > tolerance) {
            findings.push({
              type: "overflow",
              severity: "warning",
              category: "layout",
              parent: { selector: item.selector, overflow: pOverflow },
              child: { selector: childData.selector },
              direction: "bottom",
              amount: Math.round(bottomOverflow),
              context: `Child extends ${Math.round(bottomOverflow)}px beyond parent bottom edge`,
            });
          }
        }
      }
    }

    // --- Stacking / Z-Index Issues ---
    if (checks.includes("stacking")) {
      const positioned = subset.filter(
        (d) => d.style.position === "absolute" || d.style.position === "fixed",
      );
      for (let i = 0; i < positioned.length; i++) {
        for (let j = i + 1; j < positioned.length; j++) {
          const a = positioned[i],
            b = positioned[j];
          // Check overlap
          const overlapX =
            Math.min(a.rect.right, b.rect.right) - Math.max(a.rect.left, b.rect.left);
          const overlapY =
            Math.min(a.rect.bottom, b.rect.bottom) - Math.max(a.rect.top, b.rect.top);
          if (overlapX <= 0 || overlapY <= 0) continue;

          const zA = parseInt(a.style.zIndex, 10) || 0;
          const zB = parseInt(b.style.zIndex, 10) || 0;
          // Flag if lower-z element is interactive (hidden behind higher-z)
          if (zA < zB && a.isInteractive) {
            findings.push({
              type: "stacking",
              severity: "error",
              category: "layout",
              obscuredElement: { selector: a.selector, zIndex: zA, isInteractive: true },
              obscuringElement: { selector: b.selector, zIndex: zB },
              context: `Interactive element (z-index:${zA}) hidden behind element (z-index:${zB})`,
            });
          } else if (zB < zA && b.isInteractive) {
            findings.push({
              type: "stacking",
              severity: "error",
              category: "layout",
              obscuredElement: { selector: b.selector, zIndex: zB, isInteractive: true },
              obscuringElement: { selector: a.selector, zIndex: zA },
              context: `Interactive element (z-index:${zB}) hidden behind element (z-index:${zA})`,
            });
          }
        }
      }
    }

    // --- Viewport Overflow ---
    if (checks.includes("viewport")) {
      const vw = window.innerWidth,
        vh = window.innerHeight;
      for (const item of subset) {
        if (item.style.position === "fixed" || item.style.position === "sticky") continue;
        if (item.rect.right > vw + tolerance && item.rect.width < vw) {
          findings.push({
            type: "viewportOverflow",
            severity: "warning",
            category: "layout",
            element: { selector: item.selector },
            direction: "right",
            amount: Math.round(item.rect.right - vw),
            context: `Element extends ${Math.round(item.rect.right - vw)}px beyond right viewport edge`,
          });
        }
      }
    }

    return {
      success: true,
      timestamp: Date.now(),
      url: window.location.href,
      viewportSize: { width: window.innerWidth, height: window.innerHeight },
      summary: {
        errors: findings.filter((f) => f.severity === "error").length,
        warnings: findings.filter((f) => f.severity === "warning").length,
        info: findings.filter((f) => f.severity === "info").length,
        checksRun: checks,
        elementsAnalyzed: subset.length,
        durationMs: Math.round(performance.now() - startTime),
      },
      findings,
    };
  }

  /**
   * Find the most common value in an array of numbers.
   */
  function mostCommon(arr) {
    const counts = {};
    let maxCount = 0,
      mode = arr[0];
    for (const v of arr) {
      counts[v] = (counts[v] || 0) + 1;
      if (counts[v] > maxCount) {
        maxCount = counts[v];
        mode = v;
      }
    }
    return mode;
  }

  // ==========================================================================
  // Visual Analysis - Color Analysis
  // ==========================================================================

  function analyzeColors(params) {
    const startTime = performance.now();
    const wcagLevel = params?.wcagLevel || "AA";
    const checks = params?.checks || ["contrast", "consistency", "fontConsistency"];
    const findings = [];
    const elements = collectVisibleElementData(params?.scope);
    const maxElements = params?.maxElements || 500;
    const subset = elements.slice(0, maxElements);

    // --- Contrast Ratio ---
    if (checks.includes("contrast")) {
      // Only check elements that contain direct text
      const textElements = subset.filter((d) => {
        if (!d.text) return false;
        // Check that this element has direct text nodes (not just child element text)
        for (const node of d.el.childNodes) {
          if (node.nodeType === Node.TEXT_NODE && node.textContent.trim()) return true;
        }
        return false;
      });

      for (const item of textElements) {
        const fg = parseColor(item.style.color);
        if (!fg) continue;
        const bg = getEffectiveBackgroundColor(item.el);
        const effectiveFg = fg.a < 1 ? blendColors(fg, bg) : fg;
        const ratio = contrastRatio(effectiveFg, bg);

        const fontSize = parseFloat(item.style.fontSize);
        const fontWeight = parseInt(item.style.fontWeight, 10) || 400;
        const isLargeText = fontSize >= 24 || (fontSize >= 18.66 && fontWeight >= 700);

        let required;
        if (wcagLevel === "AAA") {
          required = isLargeText ? 4.5 : 7;
        } else {
          required = isLargeText ? 3 : 4.5;
        }

        if (ratio < required) {
          findings.push({
            type: "contrast",
            severity: ratio < 3 ? "error" : "warning",
            category: "color",
            element: { selector: item.selector, text: item.text.slice(0, 60) },
            foreground: colorToString(effectiveFg),
            background: colorToString(bg),
            contrastRatio: Math.round(ratio * 100) / 100,
            requiredRatio: required,
            wcagLevel,
            isLargeText,
            context: `Contrast ${ratio.toFixed(1)}:1 fails WCAG ${wcagLevel} (requires ${required}:1)`,
          });
        }
      }
    }

    // --- Color Consistency ---
    if (checks.includes("consistency")) {
      const groups = {
        button: subset.filter((d) => d.el.matches('button, [role="button"], input[type="submit"]')),
        link: subset.filter((d) => d.el.matches('a[href], [role="link"]')),
        heading: subset.filter((d) => d.el.matches("h1, h2, h3, h4, h5, h6")),
        input: subset.filter((d) =>
          d.el.matches("input:not([type=hidden]):not([type=submit]), textarea, select"),
        ),
      };

      for (const [groupName, groupItems] of Object.entries(groups)) {
        if (groupItems.length < 2) continue;
        // Collect colors used
        const colorBuckets = {};
        for (const item of groupItems) {
          const c = parseColor(item.style.color);
          if (!c) continue;
          const key = `rgb(${c.r},${c.g},${c.b})`;
          if (!colorBuckets[key]) colorBuckets[key] = { count: 0, examples: [] };
          colorBuckets[key].count++;
          if (colorBuckets[key].examples.length < 2) colorBuckets[key].examples.push(item.selector);
        }

        const bucketKeys = Object.keys(colorBuckets);
        if (bucketKeys.length > 1) {
          // Find the dominant color
          let dominant = bucketKeys[0];
          for (const k of bucketKeys) {
            if (colorBuckets[k].count > colorBuckets[dominant].count) dominant = k;
          }
          // Report outliers
          for (const k of bucketKeys) {
            if (k === dominant) continue;
            if (
              colorBuckets[k].count <= 2 &&
              colorBuckets[dominant].count > colorBuckets[k].count * 2
            ) {
              findings.push({
                type: "colorInconsistency",
                severity: "warning",
                category: "color",
                group: groupName,
                outlierColor: k,
                dominantColor: dominant,
                outlierCount: colorBuckets[k].count,
                dominantCount: colorBuckets[dominant].count,
                examples: colorBuckets[k].examples,
                context: `${colorBuckets[k].count} ${groupName}(s) use ${k} while ${colorBuckets[dominant].count} use ${dominant}`,
              });
            }
          }
        }
      }
    }

    // --- Font Consistency ---
    if (checks.includes("fontConsistency")) {
      const headings = subset.filter((d) => d.el.matches("h1, h2, h3, h4, h5, h6"));
      // Check heading size hierarchy
      const headingSizes = {};
      for (const h of headings) {
        const level = parseInt(h.tag.charAt(1), 10);
        const size = parseFloat(h.style.fontSize);
        if (!headingSizes[level]) headingSizes[level] = [];
        headingSizes[level].push({ size, selector: h.selector });
      }
      const levels = Object.keys(headingSizes).map(Number).sort();
      for (let i = 1; i < levels.length; i++) {
        const prevAvg =
          headingSizes[levels[i - 1]].reduce((a, b) => a + b.size, 0) /
          headingSizes[levels[i - 1]].length;
        const currAvg =
          headingSizes[levels[i]].reduce((a, b) => a + b.size, 0) / headingSizes[levels[i]].length;
        if (currAvg >= prevAvg) {
          findings.push({
            type: "headingHierarchy",
            severity: "warning",
            category: "typography",
            higherLevel: `h${levels[i - 1]}`,
            lowerLevel: `h${levels[i]}`,
            higherSize: Math.round(prevAvg * 10) / 10,
            lowerSize: Math.round(currAvg * 10) / 10,
            context: `h${levels[i]} (${currAvg.toFixed(1)}px) is not smaller than h${levels[i - 1]} (${prevAvg.toFixed(1)}px)`,
          });
        }
      }
    }

    return {
      success: true,
      timestamp: Date.now(),
      url: window.location.href,
      viewportSize: { width: window.innerWidth, height: window.innerHeight },
      summary: {
        errors: findings.filter((f) => f.severity === "error").length,
        warnings: findings.filter((f) => f.severity === "warning").length,
        info: findings.filter((f) => f.severity === "info").length,
        checksRun: checks,
        elementsAnalyzed: subset.length,
        durationMs: Math.round(performance.now() - startTime),
      },
      findings,
    };
  }

  // ==========================================================================
  // Visual Analysis - Image Analysis
  // ==========================================================================

  function analyzeImages(params) {
    const startTime = performance.now();
    const checks = params?.checks || [
      "broken",
      "altText",
      "aspectRatio",
      "icons",
      "sizeOptimization",
    ];
    const distortionThreshold = params?.distortionThreshold || 5;
    const oversizeThreshold = params?.oversizeThreshold || 3;
    const findings = [];
    const root = params?.scope ? document.querySelector(params.scope) : document.body;
    if (!root) return { success: false, error: "Scope not found" };

    const images = root.querySelectorAll("img");
    const svgs = root.querySelectorAll("svg");

    // --- Broken Images ---
    if (checks.includes("broken")) {
      for (const img of images) {
        if (!img.src && !img.getAttribute("src")) continue;
        if (img.complete && (img.naturalWidth === 0 || img.naturalHeight === 0)) {
          findings.push({
            type: "brokenImage",
            severity: "error",
            category: "image",
            element: { selector: buildUniqueSelector(img), src: img.src || "" },
            context: "Image failed to load (naturalWidth=0)",
          });
        }
      }
    }

    // --- Missing Alt Text ---
    if (checks.includes("altText")) {
      for (const img of images) {
        const role = img.getAttribute("role");
        if (role === "presentation" || role === "none") continue;
        const alt = img.getAttribute("alt");
        if (alt === null || alt === undefined) {
          findings.push({
            type: "missingAltText",
            severity: "error",
            category: "image",
            element: { selector: buildUniqueSelector(img), src: img.src || "" },
            context: "Image has no alt attribute",
          });
        } else if (alt.trim() === "" && role !== "presentation" && role !== "none") {
          // Empty alt is valid for decorative images, flag as info
          findings.push({
            type: "emptyAltText",
            severity: "info",
            category: "image",
            element: { selector: buildUniqueSelector(img), src: img.src || "" },
            context: "Image has empty alt text (verify it is decorative)",
          });
        }
      }
    }

    // --- Aspect Ratio Distortion ---
    if (checks.includes("aspectRatio")) {
      for (const img of images) {
        if (!img.complete || img.naturalWidth === 0 || img.naturalHeight === 0) continue;
        const rect = img.getBoundingClientRect();
        if (rect.width === 0 || rect.height === 0) continue;
        // Skip if object-fit handles it
        const objectFit = window.getComputedStyle(img).objectFit;
        if (objectFit === "cover" || objectFit === "contain" || objectFit === "fill") continue;

        const naturalRatio = img.naturalWidth / img.naturalHeight;
        const displayedRatio = rect.width / rect.height;
        const distortion = (Math.abs(naturalRatio - displayedRatio) / naturalRatio) * 100;

        if (distortion > distortionThreshold) {
          findings.push({
            type: "imageDistortion",
            severity: "warning",
            category: "image",
            element: { selector: buildUniqueSelector(img), src: img.src || "" },
            naturalAspectRatio: Math.round(naturalRatio * 100) / 100,
            displayedAspectRatio: Math.round(displayedRatio * 100) / 100,
            distortionPercent: Math.round(distortion),
            context: `Image distorted by ${Math.round(distortion)}%`,
          });
        }
      }
    }

    // --- Icon Rendering ---
    if (checks.includes("icons")) {
      for (const svg of svgs) {
        const rect = svg.getBoundingClientRect();
        // Only check small SVGs that are likely icons
        if (rect.width > 64 || rect.height > 64) continue;
        if (rect.width === 0 && rect.height === 0) {
          findings.push({
            type: "iconNotRendered",
            severity: "warning",
            category: "image",
            element: { selector: buildUniqueSelector(svg) },
            context: "SVG icon has 0x0 dimensions (may not be rendering)",
          });
        }
      }
    }

    // --- Image Size Optimization ---
    if (checks.includes("sizeOptimization")) {
      const dpr = window.devicePixelRatio || 1;
      for (const img of images) {
        if (!img.complete || img.naturalWidth === 0) continue;
        const rect = img.getBoundingClientRect();
        if (rect.width === 0 || rect.height === 0) continue;

        const naturalArea = img.naturalWidth * img.naturalHeight;
        const displayArea = rect.width * rect.height;
        const effectiveRatio = naturalArea / (displayArea * dpr * dpr);

        if (effectiveRatio > oversizeThreshold) {
          findings.push({
            type: "oversizedImage",
            severity: "info",
            category: "image",
            element: { selector: buildUniqueSelector(img), src: img.src || "" },
            naturalSize: { width: img.naturalWidth, height: img.naturalHeight },
            displayedSize: { width: Math.round(rect.width), height: Math.round(rect.height) },
            oversizeRatio: Math.round(effectiveRatio),
            devicePixelRatio: dpr,
            context: `Image is ${Math.round(effectiveRatio)}x larger than needed (accounting for ${dpr}x DPR)`,
          });
        }
      }
    }

    return {
      success: true,
      timestamp: Date.now(),
      url: window.location.href,
      summary: {
        errors: findings.filter((f) => f.severity === "error").length,
        warnings: findings.filter((f) => f.severity === "warning").length,
        info: findings.filter((f) => f.severity === "info").length,
        checksRun: checks,
        imagesAnalyzed: images.length,
        svgsAnalyzed: svgs.length,
        durationMs: Math.round(performance.now() - startTime),
      },
      findings,
    };
  }

  // ==========================================================================
  // Visual Analysis - Combined Audit
  // ==========================================================================

  function auditPage(params) {
    const startTime = performance.now();
    const minSeverity = params?.severity || "warning";
    const severityOrder = { error: 0, warning: 1, info: 2 };
    const minLevel = severityOrder[minSeverity] ?? 1;

    const layoutResult = analyzeLayout({ scope: params?.scope, ...params?.layout });
    const colorResult = analyzeColors({ scope: params?.scope, ...params?.colors });
    const imageResult = analyzeImages({ scope: params?.scope, ...params?.images });

    let allFindings = [...layoutResult.findings, ...colorResult.findings, ...imageResult.findings];

    // Filter by minimum severity
    allFindings = allFindings.filter((f) => (severityOrder[f.severity] ?? 2) <= minLevel);

    // Sort: errors first, then warnings, then info
    allFindings.sort((a, b) => (severityOrder[a.severity] ?? 2) - (severityOrder[b.severity] ?? 2));

    return {
      success: true,
      timestamp: Date.now(),
      url: window.location.href,
      viewportSize: { width: window.innerWidth, height: window.innerHeight },
      summary: {
        errors: allFindings.filter((f) => f.severity === "error").length,
        warnings: allFindings.filter((f) => f.severity === "warning").length,
        info: allFindings.filter((f) => f.severity === "info").length,
        checksRun: [
          ...layoutResult.summary.checksRun,
          ...colorResult.summary.checksRun,
          ...imageResult.summary.checksRun,
        ],
        elementsAnalyzed: layoutResult.summary.elementsAnalyzed,
        imagesAnalyzed: imageResult.summary.imagesAnalyzed || 0,
        durationMs: Math.round(performance.now() - startTime),
      },
      findings: allFindings,
    };
  }

  // Message handler for bridge communication (named function for cleanup on reinstall)
  function handleBridgeMessage(event) {
    if (event.source !== window) return;

    const { type, action, params, requestId } = event.data || {};
    if (type !== "__QONTINUI_UI_BRIDGE_REQUEST__") return;

    let response;

    switch (action) {
      case "ping": {
        // Check if UI Bridge is available
        const hasUIBridge =
          !!window.__UI_BRIDGE__ || document.querySelector("[data-ui-id]") !== null;
        response = {
          success: true,
          available: hasUIBridge,
          version: window.__UI_BRIDGE__?.version || "2.0.0",
        };
        break;
      }
      case "getElements": {
        // Use getAllInteractiveElements which works with both UI Bridge and regular websites
        let elements = getAllInteractiveElements();

        // When includeNonInteractive is true, also discover visible non-interactive
        // elements (headings, labels, badges, etc.) for spec verification.
        if (params?.includeNonInteractive) {
          const existingIds = new Set(elements.map((el) => el.id));
          const nonInteractive = discoverNonInteractiveElements();
          let nextRef = elements.length + 1;
          for (const el of nonInteractive) {
            if (!existingIds.has(el.id)) {
              existingIds.add(el.id);
              elements.push({ ...el, ref: `@e${nextRef++}` });
            }
          }
        }

        const snapshot = getStateSnapshot();
        response = {
          success: true,
          elements: elements,
          activeStatesCount: snapshot.activeStates?.length || 0,
          transitionsCount: snapshot.transitions?.length || 0,
          // Indicate whether this is a UI Bridge instrumented page or discovered elements
          isUIBridgeInstrumented: document.querySelector("[data-ui-id]") !== null,
        };
        break;
      }
      case "getElementState":
        response = getElementState(params?.elementId);
        break;
      case "enablePicker":
        enablePicker();
        response = { success: true };
        break;
      case "disablePicker":
        disablePicker();
        response = { success: true };
        break;
      case "showOverlays":
        showOverlays();
        response = { success: true };
        break;
      case "hideOverlays":
        hideOverlays();
        response = { success: true };
        break;
      case "highlightElement":
        response = highlightElement(params?.elementId);
        break;
      case "clearHighlight":
        response = clearHighlight();
        break;
      case "executeAction":
        response = executeAction(params?.elementId, params?.action, params?.params || {});
        break;
      case "getStateSnapshot":
        response = getStateSnapshot();
        break;
      case "navigate": {
        // Navigate the current page to a new URL
        if (params?.url) {
          window.location.href = params.url;
          response = { success: true, url: params.url };
        } else {
          response = { success: false, error: "No url specified" };
        }
        break;
      }
      case "getSpecs": {
        // Read specs from the page's SpecStore singleton (if available).
        // Checks two locations:
        // 1. window.__UI_BRIDGE__.specs.getGlobalSpecStore() - set by UIBridgeProvider
        // 2. window.__QONTINUI_SPEC_STORE__ - set directly by usePageSpecs hook
        // Prefers the store with actual specs loaded (handles module singleton duplication
        // in bundled apps like Next.js where different chunks get different store instances).
        try {
          const storeFromUIBridge = window.__UI_BRIDGE__?.specs?.getGlobalSpecStore?.();
          const storeFromWindow = window.__QONTINUI_SPEC_STORE__;
          const specStore =
            storeFromUIBridge && storeFromUIBridge.count > 0
              ? storeFromUIBridge
              : storeFromWindow || storeFromUIBridge;
          if (specStore && typeof specStore.getAll === "function") {
            const allConfigs = specStore.getAll(); // Map<string, SpecConfig>
            const specs = [];
            if (allConfigs instanceof Map) {
              for (const [id, config] of allConfigs) {
                specs.push({ specId: id, config });
              }
            } else if (typeof allConfigs === "object" && allConfigs !== null) {
              for (const [id, config] of Object.entries(allConfigs)) {
                specs.push({ specId: id, config });
              }
            }
            response = { success: true, specs };
          } else {
            response = { success: true, specs: [] };
          }
        } catch (e) {
          response = {
            success: false,
            error: "Failed to read specs: " + (e.message || String(e)),
          };
        }
        break;
      }
      case "analyzeLayout":
        response = analyzeLayout(params);
        break;
      case "analyzeColors":
        response = analyzeColors(params);
        break;
      case "analyzeImages":
        response = analyzeImages(params);
        break;
      case "auditPage":
        response = auditPage(params);
        break;
      default:
        response = { success: false, error: `Unknown action: ${action}` };
    }

    window.postMessage(
      {
        type: "__QONTINUI_UI_BRIDGE_RESPONSE__",
        requestId,
        success: response.success !== false,
        data: response,
        error: response.error,
      },
      "*",
    );
  }

  // Listen for messages from bridge
  window.addEventListener("message", handleBridgeMessage);

  // Expose direct command handler for chrome.scripting.executeScript calls.
  // This bypasses the ISOLATED world bridge entirely, making commands robust
  // against content script context invalidation.
  window.__qontinuiInspectorCommand = function (action, params) {
    // Reuse the same switch logic as handleBridgeMessage but return directly
    switch (action) {
      case "ping": {
        const hasUIBridge =
          !!window.__UI_BRIDGE__ || document.querySelector("[data-ui-id]") !== null;
        return {
          success: true,
          available: hasUIBridge,
          version: window.__UI_BRIDGE__?.version || "2.0.0",
        };
      }
      case "getElements": {
        let elements = getAllInteractiveElements();
        if (params?.includeNonInteractive) {
          const existingIds = new Set(elements.map((el) => el.id));
          const nonInteractive = discoverNonInteractiveElements();
          let nextRef = elements.length + 1;
          for (const el of nonInteractive) {
            if (!existingIds.has(el.id)) {
              existingIds.add(el.id);
              elements.push({ ...el, ref: `@e${nextRef++}` });
            }
          }
        }
        const snapshot = getStateSnapshot();
        return {
          success: true,
          elements: elements,
          activeStatesCount: snapshot.activeStates?.length || 0,
          transitionsCount: snapshot.transitions?.length || 0,
          isUIBridgeInstrumented: document.querySelector("[data-ui-id]") !== null,
        };
      }
      case "getElementState":
        return getElementState(params?.elementId);
      case "enablePicker":
        enablePicker();
        return { success: true };
      case "disablePicker":
        disablePicker();
        return { success: true };
      case "showOverlays":
        showOverlays();
        return { success: true };
      case "hideOverlays":
        hideOverlays();
        return { success: true };
      case "highlightElement":
        return highlightElement(params?.elementId);
      case "clearHighlight":
        return clearHighlight();
      case "executeAction":
        return executeAction(params?.elementId, params?.action, params?.params || {});
      case "getStateSnapshot":
        return getStateSnapshot();
      case "navigate":
        if (params?.url) {
          window.location.href = params.url;
          return { success: true, url: params.url };
        }
        return { success: false, error: "No url specified" };
      case "getSpecs": {
        try {
          const storeFromUIBridge = window.__UI_BRIDGE__?.specs?.getGlobalSpecStore?.();
          const storeFromWindow = window.__QONTINUI_SPEC_STORE__;
          const specStore =
            storeFromUIBridge && storeFromUIBridge.count > 0
              ? storeFromUIBridge
              : storeFromWindow || storeFromUIBridge;
          if (specStore && typeof specStore.getAll === "function") {
            const allConfigs = specStore.getAll();
            const specs = [];
            if (allConfigs instanceof Map) {
              for (const [id, config] of allConfigs) {
                specs.push({ specId: id, config });
              }
            } else if (typeof allConfigs === "object" && allConfigs !== null) {
              for (const [id, config] of Object.entries(allConfigs)) {
                specs.push({ specId: id, config });
              }
            }
            return { success: true, specs };
          }
          return { success: true, specs: [] };
        } catch (e) {
          return { success: false, error: "Failed to read specs: " + (e.message || String(e)) };
        }
      }
      case "analyzeLayout":
        return analyzeLayout(params);
      case "analyzeColors":
        return analyzeColors(params);
      case "analyzeImages":
        return analyzeImages(params);
      case "auditPage":
        return auditPage(params);
      default:
        return { success: false, error: `Unknown action: ${action}` };
    }
  };

  // Store cleanup function for extension reinstall handling
  window.__qontinuiUIBridgeInspectorCleanup = function () {
    window.removeEventListener("message", handleBridgeMessage);
    console.log("[Qontinui UI Bridge] Old inspector listener cleaned up for reinstall");
  };

  // Inject styles
  const style = document.createElement("style");
  style.textContent = `
    .__qontinui-overlay__,
    .__qontinui-highlight__ {
      pointer-events: none !important;
      box-sizing: border-box !important;
    }
  `;
  document.head.appendChild(style);

  console.log("[Qontinui UI Bridge] Inspector loaded");
})();
