/**
 * UI Bridge Inspector Content Script (MAIN World)
 *
 * Provides element inspection, picking, and overlay functionality
 * for UI Bridge elements in the page.
 */

(function () {
  'use strict';

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
  function getIframeInfo() {  // eslint-disable-line no-unused-vars
    const iframes = document.querySelectorAll('iframe');
    return Array.from(iframes).map(iframe => {
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
        src: iframe.src || '',
        name: iframe.name || '',
        id: iframe.id || '',
      };
    });
  }

  // Style constants
  const OVERLAY_COLOR = 'rgba(59, 130, 246, 0.3)';
  const OVERLAY_BORDER = 'rgba(59, 130, 246, 0.8)';
  const HOVER_COLOR = 'rgba(59, 130, 246, 0.2)';
  const LABEL_BG = 'rgba(59, 130, 246, 0.9)';

  /**
   * Check if an element is interactive
   * Returns true for elements that users can interact with
   */
  function isInteractive(el) {
    const tag = el.tagName.toLowerCase();

    // Check native interactive elements
    if (['button', 'input', 'select', 'textarea', 'a'].includes(tag)) {
      // Links need href to be interactive
      if (tag === 'a' && !el.hasAttribute('href')) {
        return false;
      }
      return true;
    }

    // Check ARIA roles that indicate interactivity
    const role = el.getAttribute('role');
    const interactiveRoles = [
      'button', 'link', 'checkbox', 'radio', 'switch',
      'tab', 'menuitem', 'menuitemcheckbox', 'menuitemradio',
      'option', 'combobox', 'listbox', 'slider', 'spinbutton',
      'textbox', 'searchbox', 'treeitem', 'gridcell'
    ];
    if (role && interactiveRoles.includes(role)) {
      return true;
    }

    // Check tabindex >= 0
    const tabindex = el.getAttribute('tabindex');
    if (tabindex !== null && parseInt(tabindex, 10) >= 0) {
      return true;
    }

    // Check onclick handler
    if (el.hasAttribute('onclick') || el.onclick) {
      return true;
    }

    // Check contenteditable
    if (el.isContentEditable || el.getAttribute('contenteditable') === 'true') {
      return true;
    }

    return false;
  }

  /**
   * Get all registered UI Bridge elements (with data-ui-id)
   */
  function getRegisteredElements() {
    const elements = document.querySelectorAll('[data-ui-id]');
    return Array.from(elements).map((el) => {
      const rect = el.getBoundingClientRect();
      const accessibility = getElementAccessibility(el);
      const interactive = isInteractive(el);
      const extraction = getExtractionAttributes(el);

      // Check required: HTML attribute or aria-required
      const htmlRequired = el.hasAttribute('required') || el.required === true;
      const ariaRequired = el.getAttribute('aria-required') === 'true';

      // Check readonly: HTML attribute or aria-readonly
      const htmlReadonly = el.hasAttribute('readonly') || el.readOnly === true;
      const ariaReadonly = el.getAttribute('aria-readonly') === 'true';

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
        value: 'value' in el ? el.value : undefined,
        checked: 'checked' in el ? el.checked : undefined,
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
    const dataUiId = el.dataset?.uiId || el.getAttribute('data-ui-id');
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
    const ariaLabel = el.getAttribute('aria-label');
    if (ariaLabel) {
      const sanitized = ariaLabel.toLowerCase().replace(/[^a-z0-9]+/g, '-').slice(0, 30);
      return `aria-${sanitized}`;
    }

    // Use text content for buttons/links (sanitized)
    const text = el.textContent?.trim();
    if (text && ['button', 'a'].includes(el.tagName.toLowerCase())) {
      const sanitized = text.toLowerCase().replace(/[^a-z0-9]+/g, '-').slice(0, 30);
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
      'button',
      'a[href]',
      'input:not([type="hidden"])',
      'select',
      'textarea',
      '[role="button"]',
      '[role="link"]',
      '[role="tab"]',
      '[role="menuitem"]',
      '[role="checkbox"]',
      '[role="radio"]',
      '[role="switch"]',
      '[role="option"]',
      '[tabindex]:not([tabindex="-1"])',
      '[onclick]',
      '[data-action]',
      '[data-click]',
    ];

    const selector = interactiveSelectors.join(', ');
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
      const htmlRequired = el.hasAttribute('required') || el.required === true;
      const ariaRequired = el.getAttribute('aria-required') === 'true';

      // Check readonly: HTML attribute or aria-readonly
      const htmlReadonly = el.hasAttribute('readonly') || el.readOnly === true;
      const ariaReadonly = el.getAttribute('aria-readonly') === 'true';

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
        value: 'value' in el ? el.value : undefined,
        checked: 'checked' in el ? el.checked : undefined,
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
        selector += '.' + classes.map(c => CSS.escape(c)).join('.');
      }

      // Add nth-child if needed for uniqueness
      const parent = current.parentElement;
      if (parent) {
        const siblings = Array.from(parent.children).filter(
          s => s.tagName === current.tagName
        );
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

    return parts.join(' > ');
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
      if (elementId.startsWith('name-')) {
        const name = elementId.slice(5);
        el = document.querySelector(`[name="${CSS.escape(name)}"]`);
        if (el) return el;
      }

      if (elementId.startsWith('aria-')) {
        // Search by aria-label prefix match
        const searchText = elementId.slice(5).replace(/-/g, ' ').trim();
        const elements = document.querySelectorAll('[aria-label]');
        for (const candidate of elements) {
          const label = candidate.getAttribute('aria-label')?.toLowerCase().replace(/[^a-z0-9]+/g, ' ').trim();
          if (label && label.startsWith(searchText.toLowerCase())) {
            return candidate;
          }
        }
      }

      if (elementId.startsWith('button-') || elementId.startsWith('a-')) {
        // Search by text content for buttons/links
        const [tag, ...textParts] = elementId.split('-');
        const searchText = textParts.join('-').replace(/-/g, ' ').trim();
        const elements = document.querySelectorAll(tag);
        for (const candidate of elements) {
          const text = candidate.textContent?.trim().toLowerCase().replace(/[^a-z0-9]+/g, ' ').trim();
          if (text && text.startsWith(searchText.toLowerCase())) {
            return candidate;
          }
        }
      }
    } catch (e) {
      console.warn('[Qontinui] Error finding element by ID:', e);
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
    return Array.from(el.querySelectorAll('[data-ui-id]'))
      .filter((child) => child.parentElement.closest('[data-ui-id]') === el)
      .map((child) => child.dataset.uiId);
  }

  /**
   * Get element label from various sources
   */
  function getElementLabel(el) {
    // Check aria-label
    if (el.getAttribute('aria-label')) {
      return el.getAttribute('aria-label');
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
    if (['button', 'a'].includes(el.tagName.toLowerCase())) {
      return el.textContent?.trim().slice(0, 50);
    }
    return undefined;
  }

  /**
   * Infer element type from tag and attributes
   */
  function inferElementType(el) {
    const role = el.getAttribute('role');
    if (role) {
      switch (role) {
        case 'button':
          return 'button';
        case 'textbox':
          return 'input';
        case 'checkbox':
          return 'checkbox';
        case 'radio':
          return 'radio';
        case 'link':
          return 'link';
        case 'listbox':
        case 'combobox':
          return 'select';
        case 'menu':
          return 'menu';
        case 'menuitem':
          return 'menuitem';
        case 'tab':
          return 'tab';
        case 'dialog':
          return 'dialog';
      }
    }

    const tag = el.tagName.toLowerCase();
    switch (tag) {
      case 'button':
        return 'button';
      case 'input': {
        const type = el.type;
        if (type === 'checkbox') return 'checkbox';
        if (type === 'radio') return 'radio';
        if (type === 'submit' || type === 'button') return 'button';
        return 'input';
      }
      case 'textarea':
        return 'textarea';
      case 'select':
        return 'select';
      case 'a':
        return 'link';
      case 'form':
        return 'form';
      default:
        return 'custom';
    }
  }

  /**
   * Infer available actions based on element type
   */
  function inferActions(type) {
    const baseActions = ['focus', 'blur', 'hover'];

    switch (type) {
      case 'button':
        return [...baseActions, 'click', 'doubleClick', 'rightClick'];
      case 'input':
        return [...baseActions, 'click', 'type', 'clear'];
      case 'textarea':
        return [...baseActions, 'click', 'type', 'clear'];
      case 'select':
        return [...baseActions, 'click', 'select'];
      case 'checkbox':
        return [...baseActions, 'click', 'check', 'uncheck', 'toggle'];
      case 'radio':
        return [...baseActions, 'click', 'check'];
      case 'link':
        return [...baseActions, 'click'];
      default:
        return [...baseActions, 'click'];
    }
  }

  /**
   * Check if element is visible
   */
  function isElementVisible(el) {
    const style = window.getComputedStyle(el);
    if (style.display === 'none') return false;
    if (style.visibility === 'hidden') return false;
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
    if ('disabled' in el && el.disabled) {
      return true;
    }
    if (el.getAttribute('aria-disabled') === 'true') {
      return true;
    }
    return false;
  }

  /**
   * Build XPath for an element
   */
  function buildXPath(el) {
    if (!el || el.nodeType !== Node.ELEMENT_NODE) {
      return '';
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

    return '/' + parts.join('/');
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
      href: el.href || el.getAttribute('href') || undefined,
      src: el.src || el.getAttribute('src') || undefined,
      alt: el.alt || el.getAttribute('alt') || undefined,
      title: el.title || el.getAttribute('title') || undefined,
      placeholder: el.placeholder || el.getAttribute('placeholder') || undefined,
      name: el.name || el.getAttribute('name') || undefined,
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
      if (attr.name.startsWith('data-') && attr.name !== 'data-ui-id') {
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
          s => s.tagName === current.tagName
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

    return parts.join(' > ');
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
    if (position === 'fixed' || position === 'sticky') {
      if (rect.top < viewportHeight * 0.2) {
        return 'fixed-top';
      } else if (rect.bottom > viewportHeight * 0.8) {
        return 'fixed-bottom';
      }
    }

    // Check if element is in a modal/dialog
    const dialog = el.closest('dialog, [role="dialog"], [role="alertdialog"], [aria-modal="true"]');
    if (dialog) {
      return 'modal';
    }

    // Calculate relative position
    const centerX = (rect.left + rect.right) / 2;
    const centerY = (rect.top + rect.bottom) / 2;

    const relativeX = centerX / viewportWidth;
    const relativeY = centerY / viewportHeight;

    // Determine zone based on position
    if (relativeY < 0.15) {
      return 'header';
    } else if (relativeY > 0.85) {
      return 'footer';
    } else if (relativeX < 0.2) {
      return 'sidebar-left';
    } else if (relativeX > 0.8) {
      return 'sidebar-right';
    } else {
      return 'main';
    }
  }

  /**
   * Get the nearest ARIA landmark context for an element.
   * Returns the landmark role and optional label.
   */
  function getLandmarkContext(el) {
    const landmarkRoles = [
      'banner', 'navigation', 'main', 'complementary',
      'contentinfo', 'search', 'form', 'region'
    ];

    // Note: landmarkSelectors kept for potential future DOM-based lookup
    const _landmarkSelectors = [
      'header', 'nav', 'main', 'aside', 'footer',
      '[role="banner"]', '[role="navigation"]', '[role="main"]',
      '[role="complementary"]', '[role="contentinfo"]', '[role="search"]',
      '[role="form"]', '[role="region"]'
    ];

    // Find nearest landmark
    let current = el;
    while (current && current !== document.body) {
      // Check explicit role
      const role = current.getAttribute('role');
      if (role && landmarkRoles.includes(role)) {
        const label = current.getAttribute('aria-label') ||
                      current.getAttribute('aria-labelledby') || '';
        return { role, label: label.slice(0, 50) };
      }

      // Check implicit landmark (semantic elements)
      const tagName = current.tagName.toLowerCase();
      const implicitMap = {
        'header': 'banner',
        'nav': 'navigation',
        'main': 'main',
        'aside': 'complementary',
        'footer': 'contentinfo'
      };

      if (implicitMap[tagName]) {
        // header/footer are only landmarks if not inside article/aside/main/nav/section
        if (tagName === 'header' || tagName === 'footer') {
          const sectioning = current.closest('article, aside, main, nav, section');
          if (!sectioning || sectioning === current) {
            const label = current.getAttribute('aria-label') || '';
            return { role: implicitMap[tagName], label: label.slice(0, 50) };
          }
        } else {
          const label = current.getAttribute('aria-label') || '';
          return { role: implicitMap[tagName], label: label.slice(0, 50) };
        }
      }

      current = current.parentElement;
    }

    return { role: 'none', label: '' };
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
      return 'icon';
    } else if (rect.width <= 150 && rect.height <= 50) {
      return 'button';
    } else if (relativeArea < 0.02) {
      return 'small';
    } else if (relativeArea < 0.1) {
      return 'medium';
    } else if (relativeArea < 0.3) {
      return 'large';
    } else if (rect.width > window.innerWidth * 0.9) {
      return 'fullwidth';
    } else {
      return 'panel';
    }
  }

  /**
   * Normalize accessible name for fingerprinting.
   * Removes dynamic content patterns and normalizes text.
   */
  function normalizeAccessibleName(name) {
    if (!name) return '';

    let normalized = name
      .toLowerCase()
      .trim()
      // Remove common dynamic patterns
      .replace(/\d{1,2}:\d{2}(:\d{2})?(\s*[ap]m)?/gi, '[time]')  // Times
      .replace(/\d{1,2}\/\d{1,2}\/\d{2,4}/g, '[date]')           // Dates MM/DD/YYYY
      .replace(/\d{4}-\d{2}-\d{2}/g, '[date]')                   // Dates YYYY-MM-DD
      .replace(/\$[\d,.]+/g, '[price]')                          // Prices
      .replace(/€[\d,.]+/g, '[price]')
      .replace(/£[\d,.]+/g, '[price]')
      .replace(/\d+(\.\d+)?%/g, '[percent]')                     // Percentages
      .replace(/\b\d{5,}\b/g, '[number]')                        // Large numbers (IDs, etc.)
      .replace(/[a-f0-9]{8,}/gi, '[hash]')                       // Hex hashes
      .replace(/\s+/g, ' ')                                      // Normalize whitespace
      .slice(0, 100);                                            // Limit length

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
    const listContainers = ['ul', 'ol', 'menu', 'datalist'];
    const parentTag = parent.tagName.toLowerCase();

    if (listContainers.includes(parentTag)) {
      const siblings = Array.from(parent.children).filter(
        s => s.tagName === el.tagName
      );
      if (siblings.length > 1) {
        return {
          type: 'list',
          containerTag: parentTag,
          index: siblings.indexOf(el),
          count: siblings.length,
          itemSelector: `${parentTag} > ${el.tagName.toLowerCase()}`
        };
      }
    }

    // Check for ARIA list roles
    const parentRole = parent.getAttribute('role');
    if (['list', 'listbox', 'menu', 'menubar', 'tablist', 'tree', 'grid'].includes(parentRole)) {
      const itemRoles = ['listitem', 'option', 'menuitem', 'menuitemcheckbox',
                         'menuitemradio', 'tab', 'treeitem', 'row', 'gridcell'];
      const elRole = el.getAttribute('role');
      if (elRole && itemRoles.includes(elRole)) {
        const siblings = Array.from(parent.children).filter(
          s => s.getAttribute('role') === elRole
        );
        return {
          type: parentRole,
          containerRole: parentRole,
          index: siblings.indexOf(el),
          count: siblings.length,
          itemRole: elRole
        };
      }
    }

    // Check for table structure
    if (el.tagName.toLowerCase() === 'tr') {
      const table = el.closest('table, [role="grid"], [role="table"]');
      if (table) {
        const rows = table.querySelectorAll('tr, [role="row"]');
        return {
          type: 'table-row',
          index: Array.from(rows).indexOf(el),
          count: rows.length
        };
      }
    }

    // Check for grid-like layout (many similar siblings)
    const similarSiblings = Array.from(parent.children).filter(s => {
      if (s === el) return true;
      // Check if sibling has same tag and similar classes
      if (s.tagName !== el.tagName) return false;
      const elClasses = Array.from(el.classList);
      const sibClasses = Array.from(s.classList);
      // At least 50% class overlap
      const overlap = elClasses.filter(c => sibClasses.includes(c)).length;
      return overlap >= Math.min(elClasses.length, sibClasses.length) * 0.5;
    });

    if (similarSiblings.length >= 3) {
      return {
        type: 'grid-item',
        index: similarSiblings.indexOf(el),
        count: similarSiblings.length
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
      hash = ((hash << 5) + hash) + str.charCodeAt(i);
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
    const role = el.getAttribute('role') || getImplicitRole(el) || el.tagName.toLowerCase();
    const accessibleName = computeAccessibleName(el) || '';
    const structuralPath = getStructuralPath(el);
    const positionZone = getPositionZone(el);
    const landmarkContext = getLandmarkContext(el);
    const sizeCategory = getSizeCategory(el);
    const repeatPattern = detectRepeatPattern(el);

    // Calculate relative position (0-1 percentage of viewport)
    const relativePosition = {
      top: Math.round((rect.top / window.innerHeight) * 100) / 100,
      left: Math.round((rect.left / window.innerWidth) * 100) / 100
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
      hash: null // Will be computed below
    };

    // Compute hash from stable properties
    const hashInput = [
      fingerprint.structuralPath,
      fingerprint.positionZone,
      fingerprint.landmarkContext,
      fingerprint.role,
      fingerprint.tagName,
      fingerprint.accessibleName || '',
      fingerprint.sizeCategory,
      // Include relative position with some tolerance (round to 0.1)
      Math.round(relativePosition.top * 10),
      Math.round(relativePosition.left * 10)
    ].join('|');

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
    a: (el) => el.hasAttribute('href') ? 'link' : null,
    article: () => 'article',
    aside: () => 'complementary',
    button: () => 'button',
    datalist: () => 'listbox',
    details: () => 'group',
    dialog: () => 'dialog',
    fieldset: () => 'group',
    figure: () => 'figure',
    footer: (el) => isLandmark(el) ? 'contentinfo' : null,
    form: (el) => el.hasAttribute('aria-label') || el.hasAttribute('aria-labelledby') || el.hasAttribute('name') ? 'form' : null,
    h1: () => 'heading',
    h2: () => 'heading',
    h3: () => 'heading',
    h4: () => 'heading',
    h5: () => 'heading',
    h6: () => 'heading',
    header: (el) => isLandmark(el) ? 'banner' : null,
    hr: () => 'separator',
    img: (el) => el.getAttribute('alt') === '' ? 'presentation' : 'img',
    input: (el) => getInputRole(el),
    li: () => 'listitem',
    main: () => 'main',
    menu: () => 'list',
    meter: () => 'meter',
    nav: () => 'navigation',
    ol: () => 'list',
    optgroup: () => 'group',
    option: () => 'option',
    output: () => 'status',
    progress: () => 'progressbar',
    section: (el) => el.hasAttribute('aria-label') || el.hasAttribute('aria-labelledby') ? 'region' : null,
    select: (el) => el.hasAttribute('multiple') || el.size > 1 ? 'listbox' : 'combobox',
    summary: () => 'button',
    table: () => 'table',
    tbody: () => 'rowgroup',
    td: () => 'cell',
    textarea: () => 'textbox',
    tfoot: () => 'rowgroup',
    th: (el) => el.getAttribute('scope') === 'col' ? 'columnheader' : 'rowheader',
    thead: () => 'rowgroup',
    tr: () => 'row',
    ul: () => 'list',
  };

  /**
   * Check if element is a landmark (not inside article/aside/main/nav/section)
   */
  function isLandmark(el) {
    const sectioning = el.closest('article, aside, main, nav, section');
    return !sectioning || sectioning === el;
  }

  /**
   * Get implicit role for input elements based on type
   */
  function getInputRole(el) {
    const type = (el.type || 'text').toLowerCase();
    switch (type) {
      case 'button':
      case 'image':
      case 'reset':
      case 'submit':
        return 'button';
      case 'checkbox':
        return 'checkbox';
      case 'email':
      case 'tel':
      case 'text':
      case 'url':
        return el.hasAttribute('list') ? 'combobox' : 'textbox';
      case 'number':
        return 'spinbutton';
      case 'radio':
        return 'radio';
      case 'range':
        return 'slider';
      case 'search':
        return el.hasAttribute('list') ? 'combobox' : 'searchbox';
      default:
        return 'textbox';
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
    const labelledBy = el.getAttribute('aria-labelledby');
    if (labelledBy) {
      const names = labelledBy.split(/\s+/)
        .map(id => {
          const labelEl = document.getElementById(id);
          return labelEl ? labelEl.textContent?.trim() : null;
        })
        .filter(Boolean);
      if (names.length > 0) {
        return names.join(' ');
      }
    }

    // Step 2: aria-label
    const ariaLabel = el.getAttribute('aria-label');
    if (ariaLabel) {
      return ariaLabel;
    }

    // Step 3: Native label association (for form elements)
    if (el instanceof HTMLInputElement || el instanceof HTMLSelectElement || el instanceof HTMLTextAreaElement) {
      // Check for <label for="id">
      if (el.id) {
        const label = document.querySelector(`label[for="${CSS.escape(el.id)}"]`);
        if (label) {
          return label.textContent?.trim() || null;
        }
      }
      // Check for wrapping <label>
      const parentLabel = el.closest('label');
      if (parentLabel) {
        // Get text excluding the input itself
        const clone = parentLabel.cloneNode(true);
        const inputs = clone.querySelectorAll('input, select, textarea');
        inputs.forEach(input => input.remove());
        const text = clone.textContent?.trim();
        if (text) return text;
      }
    }

    // Step 4: alt attribute (for images)
    if (el instanceof HTMLImageElement && el.alt) {
      return el.alt;
    }

    // Step 5: title attribute
    const title = el.getAttribute('title');
    if (title) {
      return title;
    }

    // Step 6: placeholder (for inputs)
    if ((el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) && el.placeholder) {
      return el.placeholder;
    }

    // Step 7: Text content (for buttons, links, etc.)
    const role = el.getAttribute('role') || getImplicitRole(el);
    if (['button', 'link', 'menuitem', 'tab', 'option'].includes(role)) {
      return el.textContent?.trim() || null;
    }

    return null;
  }

  /**
   * Compute the accessible description for an element
   */
  function computeAccessibleDescription(el) {
    // aria-describedby
    const describedBy = el.getAttribute('aria-describedby');
    if (describedBy) {
      const descriptions = describedBy.split(/\s+/)
        .map(id => {
          const descEl = document.getElementById(id);
          return descEl ? descEl.textContent?.trim() : null;
        })
        .filter(Boolean);
      if (descriptions.length > 0) {
        return descriptions.join(' ');
      }
    }

    // title attribute (if not used for accessible name)
    const title = el.getAttribute('title');
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
    if (tag === 'a' && el.hasAttribute('href')) return true;

    // Buttons
    if (tag === 'button') return true;

    // Form elements (unless disabled)
    if (['input', 'select', 'textarea'].includes(tag) && !el.disabled) return true;

    // Summary elements
    if (tag === 'summary') return true;

    // Elements with contenteditable
    if (el.isContentEditable) return true;

    return false;
  }

  /**
   * Get the element's tabindex value
   */
  function getTabIndex(el) {
    const tabindexAttr = el.getAttribute('tabindex');
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
    if (el.getAttribute('aria-hidden') === 'true') return false;

    // Check visibility
    if (!isElementVisible(el)) return false;

    return tabIndex >= 0 || isNaturallyFocusable(el);
  }

  /**
   * Parse aria-checked attribute value
   */
  function parseAriaChecked(value) {
    if (value === 'true') return true;
    if (value === 'false') return false;
    if (value === 'mixed') return 'mixed';
    return undefined;
  }

  /**
   * Parse aria-pressed attribute value
   */
  function parseAriaPressed(value) {
    if (value === 'true') return true;
    if (value === 'false') return false;
    if (value === 'mixed') return 'mixed';
    return undefined;
  }

  /**
   * Get comprehensive accessibility information for an element
   */
  function getElementAccessibility(el) {
    const explicitRole = el.getAttribute('role');
    const implicitRole = getImplicitRole(el);
    const tabIndex = getTabIndex(el);

    return {
      role: explicitRole || implicitRole || 'generic',
      accessibleName: computeAccessibleName(el) || undefined,
      accessibleDescription: computeAccessibleDescription(el) || undefined,
      ariaLabel: el.getAttribute('aria-label') || undefined,
      ariaLabelledBy: el.getAttribute('aria-labelledby') || undefined,
      ariaDescribedBy: el.getAttribute('aria-describedby') || undefined,
      ariaExpanded: el.getAttribute('aria-expanded') === 'true' ? true :
                    el.getAttribute('aria-expanded') === 'false' ? false : undefined,
      ariaSelected: el.getAttribute('aria-selected') === 'true' ? true :
                    el.getAttribute('aria-selected') === 'false' ? false : undefined,
      ariaChecked: parseAriaChecked(el.getAttribute('aria-checked')),
      ariaPressed: parseAriaPressed(el.getAttribute('aria-pressed')),
      ariaReadOnly: el.getAttribute('aria-readonly') === 'true' ? true :
                    el.getAttribute('aria-readonly') === 'false' ? false : undefined,
      ariaHidden: el.getAttribute('aria-hidden') === 'true' ? true : undefined,
      ariaDisabled: el.getAttribute('aria-disabled') === 'true' ? true : undefined,
      ariaRequired: el.getAttribute('aria-required') === 'true' ? true : undefined,
      ariaLive: ['off', 'polite', 'assertive'].includes(el.getAttribute('aria-live'))
                ? el.getAttribute('aria-live') : undefined,
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
    const overlay = document.createElement('div');
    overlay.className = '__qontinui-overlay__';
    const rect = el.getBoundingClientRect();

    Object.assign(overlay.style, {
      position: 'fixed',
      top: rect.top + 'px',
      left: rect.left + 'px',
      width: rect.width + 'px',
      height: rect.height + 'px',
      backgroundColor: color,
      border: `2px solid ${OVERLAY_BORDER}`,
      borderRadius: '2px',
      pointerEvents: 'none',
      zIndex: '999999',
      transition: 'all 0.1s ease-out',
    });

    // Add label if element has data-ui-id
    if (showLabel && el.dataset?.uiId) {
      const label = document.createElement('div');
      label.textContent = el.dataset.uiId;
      Object.assign(label.style, {
        position: 'absolute',
        top: '-20px',
        left: '0',
        backgroundColor: LABEL_BG,
        color: 'white',
        fontSize: '11px',
        fontFamily: '-apple-system, BlinkMacSystemFont, sans-serif',
        padding: '2px 6px',
        borderRadius: '2px',
        whiteSpace: 'nowrap',
        maxWidth: '200px',
        overflow: 'hidden',
        textOverflow: 'ellipsis',
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
    document.body.style.cursor = 'crosshair';

    document.addEventListener('mouseover', onPickerMouseOver, true);
    document.addEventListener('mouseout', onPickerMouseOut, true);
    document.addEventListener('click', onPickerClick, true);
    document.addEventListener('keydown', onPickerKeyDown, true);
  }

  /**
   * Disable element picker mode
   */
  function disablePicker() {
    pickerEnabled = false;
    document.body.style.cursor = '';

    document.removeEventListener('mouseover', onPickerMouseOver, true);
    document.removeEventListener('mouseout', onPickerMouseOut, true);
    document.removeEventListener('click', onPickerClick, true);
    document.removeEventListener('keydown', onPickerKeyDown, true);

    clearHoverOverlay();
  }

  function onPickerMouseOver(e) {
    if (!pickerEnabled) return;

    // Find closest UI Bridge element or use target
    const target = e.target.closest('[data-ui-id]') || e.target;
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

    const target = e.target.closest('[data-ui-id]') || e.target;
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
      value: 'value' in target ? target.value : undefined,
      text: target.textContent?.trim().slice(0, 100),
      label: getElementLabel(target),
      actions: inferActions(inferElementType(target)),
    };

    // Send selection to bridge
    window.postMessage(
      {
        type: '__QONTINUI_ELEMENT_SELECTED__',
        data: elementData,
      },
      '*'
    );

    disablePicker();
  }

  function onPickerKeyDown(e) {
    if (e.key === 'Escape') {
      disablePicker();
      window.postMessage(
        {
          type: '__QONTINUI_PICKER_CANCELLED__',
        },
        '*'
      );
    }
  }

  /**
   * Show overlays for all registered elements
   */
  function showOverlays() {
    hideOverlays();
    _overlayEnabled = true;

    const elements = document.querySelectorAll('[data-ui-id]');
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
    const existingHighlight = document.querySelector('.__qontinui-highlight__');
    if (existingHighlight) {
      existingHighlight.remove();
    }

    // Create highlight
    const highlight = createOverlay(el, 'rgba(34, 197, 94, 0.3)', true);
    highlight.className = '__qontinui-highlight__';
    highlight.style.border = '2px solid rgba(34, 197, 94, 0.8)';

    // Auto-remove after 2 seconds
    setTimeout(() => {
      highlight.remove();
    }, 2000);

    // Scroll into view
    el.scrollIntoView({ behavior: 'smooth', block: 'center' });

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
        case 'click':
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

            el.dispatchEvent(new MouseEvent('mousedown', mouseEventInit));
            el.dispatchEvent(new MouseEvent('mouseup', mouseEventInit));
            el.dispatchEvent(new MouseEvent('click', mouseEventInit));

            // Also try el.click() as a fallback for elements that expect it
            try { el.click(); } catch { /* ignore */ }
          }
          break;
        case 'doubleClick':
          el.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
          break;
        case 'rightClick':
          el.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true }));
          break;
        case 'focus':
          el.focus();
          break;
        case 'blur':
          el.blur();
          break;
        case 'hover':
          el.dispatchEvent(new MouseEvent('mouseenter', { bubbles: true }));
          el.dispatchEvent(new MouseEvent('mouseover', { bubbles: true }));
          break;
        case 'clear':
          if ('value' in el) {
            el.value = '';
            el.dispatchEvent(new Event('input', { bubbles: true }));
            el.dispatchEvent(new Event('change', { bubbles: true }));
          }
          break;
        case 'type':
        case 'fill': // Alias for 'type'
          if ('value' in el) {
            el.focus();
            if (params.clear) {
              el.value = '';
            }
            el.value = params.text || '';
            el.dispatchEvent(new Event('input', { bubbles: true }));
            el.dispatchEvent(new Event('change', { bubbles: true }));
          }
          break;
        case 'select':
          if (el.tagName === 'SELECT') {
            el.value = params.value;
            el.dispatchEvent(new Event('change', { bubbles: true }));
          }
          break;
        case 'check':
          if ('checked' in el && !el.checked) {
            el.click();
          }
          break;
        case 'uncheck':
          if ('checked' in el && el.checked) {
            el.click();
          }
          break;
        case 'toggle':
          if ('checked' in el) {
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
          value: 'value' in el ? el.value : undefined,
          checked: 'checked' in el ? el.checked : undefined,
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
        value: 'value' in el ? el.value : undefined,
        checked: 'checked' in el ? el.checked : undefined,
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
    const existingHighlight = document.querySelector('.__qontinui-highlight__');
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
    if (uiBridge && typeof uiBridge.getStateSnapshot === 'function') {
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

  // Message handler for bridge communication (named function for cleanup on reinstall)
  function handleBridgeMessage(event) {
    if (event.source !== window) return;

    const { type, action, params, requestId } = event.data || {};
    if (type !== '__QONTINUI_UI_BRIDGE_REQUEST__') return;

    let response;

    switch (action) {
      case 'ping': {
        // Check if UI Bridge is available
        const hasUIBridge = !!window.__UI_BRIDGE__ || document.querySelector('[data-ui-id]') !== null;
        response = {
          success: true,
          available: hasUIBridge,
          version: window.__UI_BRIDGE__?.version || '2.0.0',
        };
        break;
      }
      case 'getElements': {
        // Use getAllInteractiveElements which works with both UI Bridge and regular websites
        const elements = getAllInteractiveElements();
        const snapshot = getStateSnapshot();
        response = {
          success: true,
          elements: elements,
          activeStatesCount: snapshot.activeStates?.length || 0,
          transitionsCount: snapshot.transitions?.length || 0,
          // Indicate whether this is a UI Bridge instrumented page or discovered elements
          isUIBridgeInstrumented: document.querySelector('[data-ui-id]') !== null,
        };
        break;
      }
      case 'getElementState':
        response = getElementState(params?.elementId);
        break;
      case 'enablePicker':
        enablePicker();
        response = { success: true };
        break;
      case 'disablePicker':
        disablePicker();
        response = { success: true };
        break;
      case 'showOverlays':
        showOverlays();
        response = { success: true };
        break;
      case 'hideOverlays':
        hideOverlays();
        response = { success: true };
        break;
      case 'highlightElement':
        response = highlightElement(params?.elementId);
        break;
      case 'clearHighlight':
        response = clearHighlight();
        break;
      case 'executeAction':
        response = executeAction(params?.elementId, params?.action, params?.params || {});
        break;
      case 'getStateSnapshot':
        response = getStateSnapshot();
        break;
      default:
        response = { success: false, error: `Unknown action: ${action}` };
    }

    window.postMessage(
      {
        type: '__QONTINUI_UI_BRIDGE_RESPONSE__',
        requestId,
        success: response.success !== false,
        data: response,
        error: response.error,
      },
      '*'
    );
  }

  // Listen for messages from bridge
  window.addEventListener('message', handleBridgeMessage);

  // Store cleanup function for extension reinstall handling
  window.__qontinuiUIBridgeInspectorCleanup = function() {
    window.removeEventListener('message', handleBridgeMessage);
    console.log('[Qontinui UI Bridge] Old inspector listener cleaned up for reinstall');
  };

  // Inject styles
  const style = document.createElement('style');
  style.textContent = `
    .__qontinui-overlay__,
    .__qontinui-highlight__ {
      pointer-events: none !important;
      box-sizing: border-box !important;
    }
  `;
  document.head.appendChild(style);

  console.log('[Qontinui UI Bridge] Inspector loaded');
})();
