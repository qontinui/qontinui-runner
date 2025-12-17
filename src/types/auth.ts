/**
 * Authentication type definitions
 *
 * TypeScript interfaces matching Rust auth structs
 */

export interface User {
  id: string;
  email: string;
  name: string | null;
}

export interface DeviceInfo {
  device_id: string;
  device_name: string;
  platform: "windows" | "darwin" | "linux";
}

export interface LoginResponse {
  access_token: string;
  refresh_token: string;
  user: User;
  device_info: DeviceInfo;
}

export interface AuthStatus {
  authenticated: boolean;
  user: User | null;
  device_info: DeviceInfo | null;
}

export interface AuthContextValue {
  authStatus: AuthStatus | null;
  loading: boolean;
  error: string | null;
  login: (email: string, password: string) => Promise<void>;
  logout: () => Promise<void>;
  refreshAuth: () => Promise<void>;
}

export interface ConnectionInfo {
  device_id: string;
  websocket_url: string;
  http_url: string;
  user_id: string;
  is_active: boolean;
}

export interface Project {
  id: string;
  name: string;
  description: string | null;
  created_at: string;
  updated_at: string;
}
