/**
 * DOM Capture Service
 *
 * HTTP service for interacting with the DOM capture API endpoints.
 */

import type {
  DomCapture,
  DomCaptureApiResponse,
  CaptureFromExtensionRequest,
} from "../types/domCapture";

const API_BASE = "http://localhost:9876";

/**
 * Service for DOM capture operations via HTTP API.
 */
export const domCaptureService = {
  /**
   * List all DOM captures.
   * Returns captures sorted by timestamp (most recent first).
   */
  async listCaptures(): Promise<DomCapture[]> {
    const response = await fetch(`${API_BASE}/dom/captures`);
    if (!response.ok) {
      throw new Error(`Failed to list DOM captures: ${response.statusText}`);
    }
    const result: DomCaptureApiResponse<DomCapture[]> = await response.json();
    if (!result.success) {
      throw new Error(result.error || "Failed to list DOM captures");
    }
    return result.data || [];
  },

  /**
   * Get a specific DOM capture by ID.
   * @param id - DOM capture ID
   */
  async getCapture(id: string): Promise<DomCapture> {
    const response = await fetch(`${API_BASE}/dom/captures/${id}`);
    if (!response.ok) {
      throw new Error(`Failed to get DOM capture: ${response.statusText}`);
    }
    const result: DomCaptureApiResponse<DomCapture> = await response.json();
    if (!result.success || !result.data) {
      throw new Error(result.error || "DOM capture not found");
    }
    return result.data;
  },

  /**
   * Get the HTML content of a DOM capture.
   * @param id - DOM capture ID
   */
  async getCaptureHtml(id: string): Promise<string> {
    const response = await fetch(`${API_BASE}/dom/captures/${id}/html`);
    if (!response.ok) {
      throw new Error(`Failed to get DOM capture HTML: ${response.statusText}`);
    }
    return response.text();
  },

  /**
   * Submit a DOM capture from the browser extension.
   * This is called by the extension when it captures a page.
   * @param request - Capture request with HTML content
   */
  async submitFromExtension(request: CaptureFromExtensionRequest): Promise<DomCapture> {
    const response = await fetch(`${API_BASE}/dom/receive`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify(request),
    });
    if (!response.ok) {
      throw new Error(`Failed to submit DOM capture: ${response.statusText}`);
    }
    const result: DomCaptureApiResponse<DomCapture> = await response.json();
    if (!result.success || !result.data) {
      throw new Error(result.error || "Failed to submit DOM capture");
    }
    return result.data;
  },

  /**
   * Filter captures by task run ID.
   * @param taskRunId - Task run ID to filter by
   */
  async listCapturesForTask(taskRunId: string): Promise<DomCapture[]> {
    const all = await this.listCaptures();
    return all.filter((c) => c.taskRunId === taskRunId);
  },
};

export default domCaptureService;
