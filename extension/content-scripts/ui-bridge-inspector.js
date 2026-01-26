/**
 * UI Bridge Inspector Content Script (MAIN World)
 *
 * Provides element inspection, picking, and overlay functionality
 * for UI Bridge elements in the page.
 */

(function () {
  'use strict';

  // Prevent multiple injections
  if (window.__QONTINUI_UI_BRIDGE_INSPECTOR__) {
    return;
  }
  window.__QONTINUI_UI_BRIDGE_INSPECTOR__ = true;

  // State
  let pickerEnabled = false;
  let _overlayEnabled = false;
  let hoveredElement = null;
  let _selectedElement = null;
  let hoverOverlay = null;
  const overlayElements = new Map();

  // Style constants
  const OVERLAY_COLOR = 'rgba(59, 130, 246, 0.3)';
  const OVERLAY_BORDER = 'rgba(59, 130, 246, 0.8)';
  const HOVER_COLOR = 'rgba(59, 130, 246, 0.2)';
  const LABEL_BG = 'rgba(59, 130, 246, 0.9)';

  /**
   * Get all registered UI Bridge elements (with data-ui-id)
   */
  function getRegisteredElements() {
    const elements = document.querySelectorAll('[data-ui-id]');
    return Array.from(elements).map((el) => {
      const rect = el.getBoundingClientRect();
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

    // If we have UI Bridge elements, just return those (instrumented app)
    if (uiBridgeElements.length > 0) {
      return uiBridgeElements;
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
    const result = [];

    Array.from(allElements).forEach((el, index) => {
      // Skip hidden elements
      if (!isElementVisible(el)) return;

      // Skip elements we've already processed (e.g., element matching multiple selectors)
      if (seen.has(el)) return;
      seen.add(el);

      const rect = el.getBoundingClientRect();
      const generatedId = generateElementId(el, index);

      result.push({
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
        href: el.href || undefined,
        parent: null,
        children: [],
        actions: inferActions(inferElementType(el)),
        // Flag to indicate this is a discovered element, not UI Bridge instrumented
        _discovered: true,
        // Store info to help find the element again
        _selector: buildUniqueSelector(el),
      });
    });

    return result;
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

  // Listen for messages from bridge
  window.addEventListener('message', (event) => {
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
  });

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
