/**
 * Centralized API base URL configuration for the runner frontend.
 *
 * At startup, the actual port is read from the Tauri backend via the
 * `get_api_port` command. All API calls should use these getters instead
 * of hardcoding "http://localhost:9876".
 */

let _apiPort: number = 9876;
let _apiBase: string = "http://localhost:9876";
let _wsBase: string = "ws://localhost:9876";

export function setApiPort(port: number) {
  _apiPort = port;
  _apiBase = `http://localhost:${port}`;
  _wsBase = `ws://localhost:${port}`;
}

export function getApiPort(): number {
  return _apiPort;
}

export function getApiBase(): string {
  return _apiBase;
}

export function getWsBase(): string {
  return _wsBase;
}
