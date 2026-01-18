/**
 * Content script that intercepts fetch() and XMLHttpRequest to capture request bodies.
 *
 * Chrome's webRequest API has limitations in MV3 - requestBody is often empty for
 * fetch() and XMLHttpRequest calls. This script patches these APIs to capture bodies
 * before requests are sent.
 *
 * Since this runs in the MAIN world to access page JavaScript, it uses postMessage
 * to communicate with the ISOLATED world bridge script.
 */

(function() {
  'use strict';

  // Avoid re-injecting
  if (window.__qontinuiInterceptorInstalled) {
    return;
  }
  window.__qontinuiInterceptorInstalled = true;

  /**
   * Generate a unique request ID
   */
  function generateRequestId() {
    return `qontinui-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
  }

  /**
   * Convert various body types to a string for capture
   */
  async function bodyToString(body) {
    if (body === null || body === undefined) {
      return null;
    }

    // String
    if (typeof body === 'string') {
      return body;
    }

    // URLSearchParams
    if (body instanceof URLSearchParams) {
      return body.toString();
    }

    // FormData - convert to object representation
    if (body instanceof FormData) {
      const obj = {};
      for (const [key, value] of body.entries()) {
        if (value instanceof File) {
          obj[key] = `[File: ${value.name}, ${value.type}, ${value.size} bytes]`;
        } else {
          obj[key] = value;
        }
      }
      return JSON.stringify(obj);
    }

    // Blob
    if (body instanceof Blob) {
      // Try to read as text if it's a text type
      if (body.type && (body.type.includes('text') || body.type.includes('json'))) {
        try {
          return await body.text();
        } catch {
          return `[Blob: ${body.type}, ${body.size} bytes]`;
        }
      }
      return `[Blob: ${body.type || 'unknown'}, ${body.size} bytes]`;
    }

    // ArrayBuffer or TypedArray
    if (body instanceof ArrayBuffer || ArrayBuffer.isView(body)) {
      try {
        const decoder = new TextDecoder('utf-8');
        const text = decoder.decode(body);
        // Check if it looks like text/JSON
        if (/^[\x20-\x7E\s]*$/.test(text.substring(0, 100))) {
          return text;
        }
        return `[Binary: ${body.byteLength || body.length} bytes]`;
      } catch {
        return `[Binary: ${body.byteLength || body.length} bytes]`;
      }
    }

    // ReadableStream - can't easily capture without consuming
    if (body instanceof ReadableStream) {
      return '[ReadableStream]';
    }

    // Object - stringify
    if (typeof body === 'object') {
      try {
        return JSON.stringify(body);
      } catch {
        return '[Object]';
      }
    }

    return String(body);
  }

  /**
   * Send captured request body via postMessage to the bridge script
   */
  function sendToBridge(requestInfo) {
    try {
      window.postMessage({
        type: '__QONTINUI_REQUEST_BODY__',
        data: requestInfo
      }, '*');
    } catch (e) {
      // Ignore errors
    }
  }

  // =============================================================================
  // Patch fetch()
  // =============================================================================

  const originalFetch = window.fetch;

  window.fetch = async function(input, init) {
    const requestId = generateRequestId();
    let url, method, headers, body;

    try {
      // Handle Request object or string URL
      if (input instanceof Request) {
        url = input.url;
        method = input.method;
        headers = {};
        input.headers.forEach((value, key) => {
          headers[key] = value;
        });
        // Clone to read body without consuming
        if (input.body) {
          try {
            const cloned = input.clone();
            body = await cloned.text();
          } catch {
            body = '[Unable to read Request body]';
          }
        }
      } else {
        url = String(input);
        method = init?.method || 'GET';
        headers = init?.headers instanceof Headers
          ? Object.fromEntries(init.headers.entries())
          : (init?.headers || {});
        body = await bodyToString(init?.body);
      }

      // Send to bridge
      sendToBridge({
        requestId,
        url,
        method,
        headers,
        body,
        timestamp: Date.now(),
        source: 'fetch'
      });

    } catch (e) {
      // Ignore capture errors - don't break the app
    }

    // Call original fetch
    return originalFetch.apply(this, arguments);
  };

  // =============================================================================
  // Patch XMLHttpRequest
  // =============================================================================

  const XHROpen = XMLHttpRequest.prototype.open;
  const XHRSetRequestHeader = XMLHttpRequest.prototype.setRequestHeader;
  const XHRSend = XMLHttpRequest.prototype.send;

  XMLHttpRequest.prototype.open = function(method, url) {
    this.__qontinui = {
      requestId: generateRequestId(),
      method: method,
      url: url,
      headers: {},
      timestamp: Date.now()
    };
    return XHROpen.apply(this, arguments);
  };

  XMLHttpRequest.prototype.setRequestHeader = function(name, value) {
    if (this.__qontinui) {
      this.__qontinui.headers[name] = value;
    }
    return XHRSetRequestHeader.apply(this, arguments);
  };

  XMLHttpRequest.prototype.send = function(body) {
    if (this.__qontinui) {
      (async () => {
        try {
          const bodyStr = await bodyToString(body);
          sendToBridge({
            requestId: this.__qontinui.requestId,
            url: this.__qontinui.url,
            method: this.__qontinui.method,
            headers: this.__qontinui.headers,
            body: bodyStr,
            timestamp: this.__qontinui.timestamp,
            source: 'xhr'
          });
        } catch (e) {
          // Ignore capture errors
        }
      })();
    }
    return XHRSend.apply(this, arguments);
  };
})();
