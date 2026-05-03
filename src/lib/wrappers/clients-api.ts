/**
 * Typed client for the runner's `/wrappers/clients/*` HTTP endpoints.
 *
 * Backend lives in `src-tauri/src/wrappers/clients.rs` + `routes.rs`. The
 * payload uses default (snake_case) serde naming for struct fields; the
 * `ConnectionState` enum is a tagged union with `"kind"` discriminator and
 * PascalCase tag values.
 */

import { getApiBase } from "@/lib/runner-api";
import { ensureOk, unwrapEnvelope } from "./api";

export type AiClientId = "claude-cli" | "claude-desktop" | "cursor" | "continue" | "cline";

export type ConnectionState =
  | { kind: "Connected" }
  | { kind: "Drifted"; current_command: string; current_port: number | null }
  | { kind: "NotConnected" };

export interface ClientInfo {
  id: AiClientId;
  display_name: string;
  installed: boolean;
  config_path: string | null;
  connection: ConnectionState;
}

export async function listAiClients(): Promise<ClientInfo[]> {
  const res = await fetch(`${getApiBase()}/wrappers/clients`);
  await ensureOk(res, "GET /wrappers/clients");
  const list = await unwrapEnvelope<ClientInfo[] | undefined>(res, "GET /wrappers/clients");
  return list ?? [];
}

export async function connectAiClient(id: AiClientId): Promise<void> {
  const res = await fetch(`${getApiBase()}/wrappers/clients/${id}/connect`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
  });
  await ensureOk(res, `POST /wrappers/clients/${id}/connect`);
  await unwrapEnvelope<unknown>(res, `POST /wrappers/clients/${id}/connect`);
}

export async function disconnectAiClient(id: AiClientId): Promise<void> {
  const res = await fetch(`${getApiBase()}/wrappers/clients/${id}/disconnect`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
  });
  await ensureOk(res, `POST /wrappers/clients/${id}/disconnect`);
  await unwrapEnvelope<unknown>(res, `POST /wrappers/clients/${id}/disconnect`);
}
