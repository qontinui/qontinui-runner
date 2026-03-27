/**
 * Mock fetch responses for API endpoints.
 *
 * Intercepts globalThis.fetch to return mock responses for configured
 * paths. Unmatched requests pass through to the original fetch.
 */

export interface ApiMockConfig {
  /** Map of path → response body for GET requests */
  getResponses: Record<string, unknown>;
  /** Map of path → response body for POST requests */
  postResponses?: Record<string, unknown>;
}

let originalFetch: typeof fetch | null = null;
let _activeMocks: ApiMockConfig | null = null;

/**
 * Install mock fetch. Call before rendering components.
 */
export function setupApiMocks(config: ApiMockConfig): void {
  originalFetch = globalThis.fetch;
  _activeMocks = config;

  globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const method = init?.method?.toUpperCase() ?? "GET";

    // Extract path from full URL
    let path: string;
    try {
      path = new URL(url).pathname + new URL(url).search;
    } catch {
      path = url;
    }

    // Check mocks
    const responses = method === "POST"
      ? { ...config.getResponses, ...config.postResponses }
      : config.getResponses;

    for (const [mockPath, mockBody] of Object.entries(responses)) {
      if (path === mockPath || path.startsWith(mockPath + "?") || path.startsWith(mockPath + "&")) {
        return new Response(JSON.stringify(mockBody), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        });
      }
    }

    // Pass through if original fetch exists (for actual network in test env)
    if (originalFetch) {
      return originalFetch(input, init);
    }

    // Default: return 404
    return new Response(JSON.stringify({ success: false, error: `No mock for ${method} ${path}` }), {
      status: 404,
      headers: { "Content-Type": "application/json" },
    });
  };
}

/**
 * Remove mock fetch and restore the original.
 */
export function teardownApiMocks(): void {
  if (originalFetch) {
    globalThis.fetch = originalFetch;
    originalFetch = null;
  }
  _activeMocks = null;
}

/**
 * Default mock responses for Inngest API endpoints.
 */
export const INNGEST_API_MOCKS: ApiMockConfig = {
  getResponses: {
    "/inngest/events": {
      success: true,
      data: [
        {
          name: "workflow.completed",
          data: { status: "completed" },
          timestamp: new Date().toISOString(),
          idempotency_key: "test-1",
          source: { task_run_id: "tr-001", workflow_name: "Test Workflow" },
        },
        {
          name: "workflow.failed",
          data: { status: "failed", reason: "timeout" },
          timestamp: new Date().toISOString(),
          source: { task_run_id: "tr-002", workflow_name: "Other Workflow" },
        },
      ],
    },
    "/inngest/queue": {
      success: true,
      data: { pending: 0, running: 0, max_concurrent: 3, max_queue_depth: 50, durable_persistence: true },
    },
    "/inngest/circuit-breaker": {
      success: true,
      data: { state: "closed", failure_count: 0, failure_threshold: 5, cooldown_ms: 15000 },
    },
    "/inngest/subscriptions": {
      success: true,
      data: [
        { id: "sub-1", event_pattern: "workflow.*", target_workflow_id: null, once: false },
      ],
    },
    "/inngest/flow-control": {
      success: true,
      data: { active: true, description: "Per-workflow flow control enforcer" },
    },
  },
};
