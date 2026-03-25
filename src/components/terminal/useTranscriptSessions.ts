import { useState, useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CommandResponse } from "./types";

export interface TranscriptSession {
  session_id: string;
  project_path: string;
  config_dir: string;
  message_count: number;
  last_modified: string;
  started_at: string | null;
  first_message_preview: string | null;
  has_plans: boolean;
  display_name: string;
}

export interface TranscriptMessage {
  uuid: string;
  msg_type: string;
  timestamp: string;
  text: string;
  plan_content: string | null;
  model: string | null;
  has_tool_use: boolean;
}

export interface UseTranscriptSessionsResult {
  sessions: TranscriptSession[];
  loading: boolean;
  error: string | null;
  refresh: () => void;
  loadMessages: (sessionId: string) => Promise<TranscriptMessage[]>;
}

export function useTranscriptSessions(): UseTranscriptSessionsResult {
  const [sessions, setSessions] = useState<TranscriptSession[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const didLoad = useRef(false);

  const fetchSessions = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<CommandResponse>("transcript_list_sessions");
      if (result.success && result.data) {
        setSessions(result.data as TranscriptSession[]);
      } else {
        setError(result.message || "Failed to load sessions");
      }
    } catch (err) {
      setError(`Failed to load sessions: ${err}`);
    } finally {
      setLoading(false);
    }
  }, []);

  // Load sessions on mount
  useEffect(() => {
    if (!didLoad.current) {
      didLoad.current = true;
      fetchSessions();
    }
  }, [fetchSessions]);

  const loadMessages = useCallback(async (sessionId: string): Promise<TranscriptMessage[]> => {
    try {
      const result = await invoke<CommandResponse>("transcript_read_session", {
        sessionId,
      });
      if (result.success && result.data) {
        return result.data as TranscriptMessage[];
      }
      return [];
    } catch {
      return [];
    }
  }, []);

  return {
    sessions,
    loading,
    error,
    refresh: fetchSessions,
    loadMessages,
  };
}
