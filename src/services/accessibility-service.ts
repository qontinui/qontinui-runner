/**
 * Accessibility Service
 *
 * HTTP service for interacting with the accessibility capture API.
 * The runner exposes Python executor commands via HTTP on port 9876.
 */

import type {
  AccessibilityCaptureOptions,
  AccessibilityCaptureResult,
  AccessibilityActionResult,
  CDPScanResult,
  BrowserTargetsResult,
  AutoConnectResult,
  AccessibilityFindOptions,
  AccessibilityFindResult,
  AccessibilitySnapshot,
} from "../types/accessibility";

const API_BASE = "http://localhost:9876";

/**
 * Execute a Python executor command via HTTP.
 */
async function executeCommand<T>(
  cmdType: string,
  params: Record<string, unknown> = {},
): Promise<T> {
  const response = await fetch(`${API_BASE}/execute`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      cmd_type: cmdType,
      params,
    }),
  });

  if (!response.ok) {
    throw new Error(`Failed to execute ${cmdType}: ${response.statusText}`);
  }

  const result = await response.json();
  return result as T;
}

/**
 * Service for accessibility tree capture and interaction.
 */
export const accessibilityService = {
  /**
   * Capture the accessibility tree from a browser via CDP.
   */
  async captureTree(
    options: AccessibilityCaptureOptions = {},
  ): Promise<AccessibilityCaptureResult> {
    return executeCommand<AccessibilityCaptureResult>("capture_accessibility", {
      target: options.target ?? "auto",
      cdp_host: options.cdp_host ?? "localhost",
      cdp_port: options.cdp_port ?? 9222,
      interactive_only: options.interactive_only ?? false,
      include_hidden: options.include_hidden ?? false,
      max_depth: options.max_depth,
    });
  },

  /**
   * Click an element by its ref.
   */
  async clickRef(ref: string): Promise<AccessibilityActionResult> {
    return executeCommand<AccessibilityActionResult>("click_ref", { ref });
  },

  /**
   * Type text into an element by its ref.
   */
  async fillRef(
    ref: string,
    value: string,
    clearFirst = false,
  ): Promise<AccessibilityActionResult> {
    return executeCommand<AccessibilityActionResult>("fill_ref", {
      ref,
      value,
      clear_first: clearFirst,
    });
  },

  /**
   * Focus an element by its ref.
   */
  async focusRef(ref: string): Promise<AccessibilityActionResult> {
    return executeCommand<AccessibilityActionResult>("focus_ref", { ref });
  },

  /**
   * Get element details by ref (from cached snapshot).
   */
  async getElementByRef(ref: string): Promise<{
    success: boolean;
    element?: Record<string, unknown>;
    error?: string;
  }> {
    return executeCommand("get_element_by_ref", { ref });
  },

  /**
   * Get the current cached snapshot.
   */
  async getCurrentSnapshot(): Promise<{
    success: boolean;
    snapshot?: AccessibilitySnapshot;
    stats?: {
      total_nodes: number;
      interactive_nodes: number;
      backend: string;
      url?: string;
      title?: string;
    };
    error?: string;
  }> {
    return executeCommand("get_accessibility_snapshot", {});
  },

  /**
   * Get AI-friendly context from the current snapshot.
   */
  async getAIContext(
    maxElements = 100,
    interactiveOnly = true,
  ): Promise<{
    success: boolean;
    ai_context?: string;
    error?: string;
  }> {
    return executeCommand("get_accessibility_ai_context", {
      max_elements: maxElements,
      interactive_only: interactiveOnly,
    });
  },

  /**
   * Find elements matching criteria.
   */
  async findElements(options: AccessibilityFindOptions): Promise<AccessibilityFindResult> {
    return executeCommand<AccessibilityFindResult>("find_accessibility_elements", {
      role: options.role,
      name: options.name,
      name_contains: options.name_contains,
      is_interactive: options.is_interactive,
    });
  },

  /**
   * Disconnect from the accessibility source.
   */
  async disconnect(): Promise<{ success: boolean; error?: string }> {
    return executeCommand("disconnect_accessibility", {});
  },

  /**
   * Scan common CDP ports to find available browsers.
   */
  async scanCDPPorts(host = "localhost", ports?: number[], timeout = 2.0): Promise<CDPScanResult> {
    return executeCommand<CDPScanResult>("scan_cdp_ports", {
      host,
      ports,
      timeout,
    });
  },

  /**
   * List available browser page targets on a CDP port.
   */
  async listBrowserTargets(host = "localhost", port = 9222): Promise<BrowserTargetsResult> {
    return executeCommand<BrowserTargetsResult>("list_browser_targets", {
      host,
      port,
    });
  },

  /**
   * Automatically connect to the first available CDP target.
   */
  async autoConnect(host = "localhost", preferredPorts?: number[]): Promise<AutoConnectResult> {
    return executeCommand<AutoConnectResult>("auto_connect_accessibility", {
      host,
      preferred_ports: preferredPorts,
    });
  },
};

export default accessibilityService;
