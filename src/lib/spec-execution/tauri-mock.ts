/**
 * Mock for @tauri-apps/api modules in jsdom/vitest context.
 *
 * Allows runner React components to be rendered without a real Tauri
 * backend. All invoke() calls return sensible defaults or can be
 * configured with custom mock responses.
 */

const invokeMocks: Record<string, unknown> = {};

/**
 * Register a mock response for a Tauri invoke command.
 */
export function mockInvoke(command: string, response: unknown): void {
  invokeMocks[command] = response;
}

/**
 * Clear all invoke mocks.
 */
export function clearInvokeMocks(): void {
  Object.keys(invokeMocks).forEach((k) => delete invokeMocks[k]);
}

/**
 * Setup Tauri mocks globally. Call before rendering components.
 */
export function setupTauriMocks(): void {
  // Mock @tauri-apps/api/core
  const core = {
    invoke: async (cmd: string, args?: unknown) => {
      if (cmd in invokeMocks) {
        const mock = invokeMocks[cmd];
        return typeof mock === "function" ? (mock as (a: unknown) => unknown)(args) : mock;
      }
      // Default: return null for unknown commands
      console.warn(`[tauri-mock] Unhandled invoke: ${cmd}`);
      return null;
    },
  };

  // Mock @tauri-apps/api/event
  const event = {
    listen: async (_event: string, _handler: (...args: unknown[]) => void) => {
      // Return unlisten function
      return () => {};
    },
    emit: async (_event: string, _payload?: unknown) => {},
    once: async (_event: string, _handler: (...args: unknown[]) => void) => {
      return () => {};
    },
  };

  // Mock @tauri-apps/api/window
  const window = {
    getCurrentWindow: () => ({
      listen: event.listen,
      emit: event.emit,
      setTitle: async () => {},
    }),
    Window: class {
      listen = event.listen;
      emit = event.emit;
    },
  };

  // Inject into global for module resolution
  (globalThis as unknown as Record<string, unknown>).__TAURI_MOCKS__ = { core, event, window };
}
