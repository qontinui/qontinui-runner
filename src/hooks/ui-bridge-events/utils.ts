import { getApiPort } from "@/lib/runner-api";
import {
  type RegisteredElement,
  type RegisteredComponent,
  serializeRegisteredElement,
} from "ui-bridge";
import type { SerializedElement, SerializedComponent } from "./types";

/**
 * Send a response to the Rust backend via HTTP fallback.
 * Used when the Tauri event system is unresponsive.
 */
export async function httpSendResponse(response: unknown): Promise<boolean> {
  try {
    const port = getApiPort();
    const resp = await fetch(`http://localhost:${port}/ui-bridge/ipc-response`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(response),
    });
    return resp.ok;
  } catch {
    return false;
  }
}

/**
 * Send a pong to the Rust backend via HTTP fallback.
 */
export async function httpSendPong(): Promise<boolean> {
  try {
    const port = getApiPort();
    const resp = await fetch(`http://localhost:${port}/ui-bridge/pong`, {
      method: "POST",
    });
    return resp.ok;
  } catch {
    return false;
  }
}

/**
 * Map task run status strings from the runner to SDK-compatible workflow status values.
 */
export function mapTaskRunStatus(status: string): string {
  switch (status) {
    case "in_progress":
    case "running":
      return "running";
    case "completed":
    case "complete":
    case "success":
      return "completed";
    case "failed":
    case "error":
      return "failed";
    case "cancelled":
    case "stopped":
      return "cancelled";
    default:
      return "pending";
  }
}

/**
 * Serialize a RegisteredElement to a plain object (removes DOM references)
 */
export function serializeElement(element: RegisteredElement): SerializedElement {
  // Delegate to ui-bridge's shared serializer so this path can't drift from
  // BridgeSnapshot's element shape. The runner mounts UI Bridge routes under
  // /ui-bridge/control/*, so override the default base path. Add registeredAt
  // /mounted on top — they're single-element-detail-only and not part of the
  // snapshot contract.
  const base = serializeRegisteredElement(element, {
    componentBasePath: "/ui-bridge/control/component",
  });
  return {
    ...base,
    registeredAt: element.registeredAt,
    mounted: element.mounted,
  } as SerializedElement;
}

/**
 * Serialize a RegisteredComponent to a plain object
 */
export function serializeComponent(component: RegisteredComponent): SerializedComponent {
  return {
    id: component.id,
    name: component.name,
    description: component.description,
    actions: component.actions.map((a) => ({
      id: a.id,
      label: a.label,
      description: a.description,
      paramSchema: a.paramSchema,
      path: `/ui-bridge/control/component/${component.id}/action/${a.id}`,
    })),
    actionInvocationPath: `/ui-bridge/control/component/${component.id}/action/{actionId}`,
    elementIds: component.elementIds,
    registeredAt: component.registeredAt,
    mounted: component.mounted,
  };
}

/**
 * Typed accessor for the global `window.__UI_BRIDGE__` object.
 */
export function getUIBridgeGlobal(): Record<string, unknown> | undefined {
  return (window as unknown as Record<string, unknown>).__UI_BRIDGE__ as
    | Record<string, unknown>
    | undefined;
}
