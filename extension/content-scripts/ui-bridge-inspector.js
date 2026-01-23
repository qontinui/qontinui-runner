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
  let overlayEnabled = false;
  let hoveredElement = null;
  let selectedElement = null;
  let hoverOverlay = null;
  const overlayElements = new Map();

  // Style constants
  const OVERLAY_COLOR = 'rgba(59, 130, 246, 0.3)';
  const OVERLAY_BORDER = 'rgba(59, 130, 246, 0.8)';
  const HOVER_COLOR = 'rgba(59, 130, 246, 0.2)';
  const LABEL_BG = 'rgba(59, 130, 246, 0.9)';

  /**
   * Get all registered UI Bridge elements
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

  function onPickerMouseOut(e) {
    // Keep overlay until we hover over something else
  }

  function onPickerClick(e) {
    if (!pickerEnabled) return;

    e.preventDefault();
    e.stopPropagation();

    const target = e.target.closest('[data-ui-id]') || e.target;
    selectedElement = target;

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
    overlayEnabled = true;

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
    overlayEnabled = false;
    overlayElements.forEach((overlay) => overlay.remove());
    overlayElements.clear();
  }

  /**
   * Highlight a specific element by ID
   */
  function highlightElement(elementId) {
    const el = document.querySelector(`[data-ui-id="${elementId}"]`);
    if (!el) {
      return { success: false, error: 'Element not found' };
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
    const el = document.querySelector(`[data-ui-id="${elementId}"]`);
    if (!el) {
      return { success: false, error: 'Element not found' };
    }

    try {
      switch (action) {
        case 'click':
          el.click();
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
    const el = document.querySelector(`[data-ui-id="${elementId}"]`);
    if (!el) {
      return { success: false, error: 'Element not found' };
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
        const elements = getRegisteredElements();
        const snapshot = getStateSnapshot();
        response = {
          success: true,
          elements: elements,
          activeStatesCount: snapshot.activeStates?.length || 0,
          transitionsCount: snapshot.transitions?.length || 0,
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
