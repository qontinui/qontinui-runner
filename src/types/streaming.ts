/**
 * Streaming types for Sunshine/Moonlight integration
 */

export interface StreamingStatus {
  sunshine_available: boolean;
  sunshine_url: string | null;
  sunshine_version: string | null;
  encoder: string | null;
  streams_active: number;
  host_ip: string | null;
}

export interface PairResponse {
  success: boolean;
  pin: string | null;
  sunshine_host: string | null;
  sunshine_port: number;
  error: string | null;
}

export interface FocusRequest {
  target: 'runner' | 'window' | 'desktop';
  window_title?: string;
}

export interface FocusResponse {
  success: boolean;
  target: string;
  message: string;
}

export interface ScreenshotQuery {
  quality?: number;
  monitor?: number;
  window_title?: string;
}
