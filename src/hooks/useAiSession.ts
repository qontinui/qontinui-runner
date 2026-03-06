/**
 * useAiSession Hook
 *
 * Manages a standalone AI session in the runner via Tauri IPC.
 * Used by ProcessManager's "Fix with AI" feature and similar AI interactions.
 */

import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AiMessage, AiSessionState } from "@qontinui/shared-types";
import { parseOutputLog } from "@qontinui/workflow-utils";

/** Payload from the "ai-output" Tauri event. */
interface AiOutputEvent {
  line: string;
  source: string;
  taskRunId?: string | null;
}

/** Payload from the "claude-session-state" Tauri event. */
interface SessionStateEvent {
  taskRunId: string;
  sessionId: string;
  state: string;
}

/** Standard command response from Rust backend. */
interface CommandResponse {
  success: boolean;
  message?: string;
  data?: Record<string, unknown>;
}

const STORAGE_KEY = "qontinui-ai-active-session";

export function useAiSession() {
  const [taskRunId, setTaskRunId] = useState<string | null>(null);
  const [sessionState, setSessionState] = useState<AiSessionState>("disconnected");
  const [messages, setMessages] = useState<AiMessage[]>([]);
  const [streamingContent, setStreamingContent] = useState("");
  const [isGeneratingWorkflow, setIsGeneratingWorkflow] = useState(false);
  const [toolActivity, setToolActivity] = useState<string | null>(null);

  const streamingBufferRef = useRef("");
  const taskRunIdRef = useRef<string | null>(null);
  const sessionStateRef = useRef<AiSessionState>("disconnected");
  const restoredRef = useRef(false);
  const restorePollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Keep refs in sync with state
  useEffect(() => {
    taskRunIdRef.current = taskRunId;
  }, [taskRunId]);

  useEffect(() => {
    sessionStateRef.current = sessionState;
  }, [sessionState]);

  // Restore session from localStorage on mount
  useEffect(() => {
    if (restoredRef.current) return;
    restoredRef.current = true;

    const savedId = localStorage.getItem(STORAGE_KEY);
    if (!savedId) return;

    // Check if the session is still alive in the SessionManager
    invoke<CommandResponse>("get_ai_session_state", { taskRunId: savedId })
      .then((response) => {
        const state = response.data?.state as string | undefined;

        if (state && state !== "not_found" && state !== "closed") {
          // Session is alive in SessionManager — restore normally
          setTaskRunId(savedId);
          setSessionState(state as AiSessionState);

          // Load message history from DB
          invoke<CommandResponse>("get_ai_output", { taskRunId: savedId })
            .then((outputResponse) => {
              if (outputResponse.success && outputResponse.data?.output_log) {
                const parsed = parseOutputLog(outputResponse.data.output_log as string);
                setMessages(parsed);
              }
            })
            .catch(() => {});
          return;
        }

        // Session not in SessionManager — check if DB has a running chat session
        // (it may be in the process of being resumed after a runner restart)
        invoke<CommandResponse>("get_ai_output", { taskRunId: savedId })
          .then((outputResponse) => {
            if (
              !outputResponse.success ||
              !outputResponse.data?.output_log ||
              !(outputResponse.data.output_log as string).trim()
            ) {
              // No output in DB either — session is truly gone
              localStorage.removeItem(STORAGE_KEY);
              return;
            }

            // DB has conversation history — enter restoring state
            setTaskRunId(savedId);
            setSessionState("restoring");

            // Load conversation history so user sees previous messages immediately
            const parsed = parseOutputLog(outputResponse.data.output_log as string);
            setMessages(parsed);

            // Poll for the backend to resume the session
            let pollCount = 0;
            const maxPolls = 20; // 20 × 500ms = 10 seconds
            const pollInterval = setInterval(() => {
              pollCount++;
              invoke<CommandResponse>("get_ai_session_state", {
                taskRunId: savedId,
              })
                .then((pollResponse) => {
                  const pollState = pollResponse.data?.state as string | undefined;

                  if (pollState && pollState !== "not_found" && pollState !== "closed") {
                    // Session has been resumed by the backend
                    clearInterval(pollInterval);
                    restorePollRef.current = null;
                    setSessionState(pollState as AiSessionState);
                  } else if (pollCount >= maxPolls) {
                    // Timeout — session wasn't resumed, clean up
                    clearInterval(pollInterval);
                    restorePollRef.current = null;
                    setSessionState("disconnected");
                    localStorage.removeItem(STORAGE_KEY);
                  }
                })
                .catch(() => {
                  if (pollCount >= maxPolls) {
                    clearInterval(pollInterval);
                    restorePollRef.current = null;
                    setSessionState("disconnected");
                    localStorage.removeItem(STORAGE_KEY);
                  }
                });
            }, 500);
            restorePollRef.current = pollInterval;
          })
          .catch(() => {
            localStorage.removeItem(STORAGE_KEY);
          });
      })
      .catch(() => {
        localStorage.removeItem(STORAGE_KEY);
      });

    // Cleanup polling interval on unmount
    return () => {
      if (restorePollRef.current) {
        clearInterval(restorePollRef.current);
        restorePollRef.current = null;
      }
    };
  }, []);

  // Listen to Tauri events
  useEffect(() => {
    let cancelled = false;
    const unlisteners: UnlistenFn[] = [];

    // Listen for AI output (streaming content + tool activity)
    listen<AiOutputEvent>("ai-output", (event) => {
      if (cancelled) return;
      const { line, source, taskRunId: eventTaskRunId } = event.payload;

      // Only process events for our session
      if (!taskRunIdRef.current || eventTaskRunId !== taskRunIdRef.current) return;

      // Skip user message echoes
      if (source === "user_message") return;

      // Track tool activity (e.g., "Reading file...", "Running command...")
      if (source === "tool_activity") {
        setToolActivity(line);
        return;
      }

      // System notes (e.g., workflow generation notification)
      if (source === "system_note") {
        setMessages((prev) => [
          ...prev,
          { role: "system", content: line, timestamp: new Date().toISOString() },
        ]);
        return;
      }

      // Text arriving — clear tool activity and accumulate.
      // The Rust backend strips the \n delimiter when reading stdout line-by-line,
      // so we restore it here to preserve paragraph breaks in markdown.
      setToolActivity(null);
      streamingBufferRef.current += line + "\n";
      setStreamingContent(streamingBufferRef.current);
    }).then((fn) => {
      if (cancelled) {
        fn(); // Immediately unlisten if effect was already cleaned up
      } else {
        unlisteners.push(fn);
      }
    });

    // Listen for session state changes
    listen<SessionStateEvent>("claude-session-state", (event) => {
      if (cancelled) return;
      const { taskRunId: eventTaskRunId, state } = event.payload;

      // Only process events for our session
      if (!taskRunIdRef.current || eventTaskRunId !== taskRunIdRef.current) return;

      // Flush streaming buffer on transition to ready or closed
      if ((state === "ready" || state === "closed") && streamingBufferRef.current.trim()) {
        const content = streamingBufferRef.current;
        streamingBufferRef.current = "";
        setStreamingContent("");
        setMessages((prev) => {
          // If the last message is also from AI (no user message in between),
          // merge content into that message instead of creating a new bubble.
          // This prevents a wall of repeated messages when Claude's agentic
          // loop cycles multiple turns without user interaction.
          const last = prev[prev.length - 1];
          if (last && last.role === "ai") {
            const merged = [...prev];
            merged[merged.length - 1] = {
              ...last,
              content: last.content + "\n\n" + content,
            };
            return merged;
          }
          return [...prev, { role: "ai", content, timestamp: new Date().toISOString() }];
        });
      }

      // Clear tool activity on state transitions
      if (state === "ready" || state === "closed") {
        setToolActivity(null);
      }

      setSessionState(state as AiSessionState);
    }).then((fn) => {
      if (cancelled) {
        fn();
      } else {
        unlisteners.push(fn);
      }
    });

    return () => {
      cancelled = true;
      for (const unlisten of unlisteners) {
        unlisten();
      }
    };
  }, []);

  const switchSession = useCallback(async (newTaskRunId: string) => {
    // Clear current streaming state
    streamingBufferRef.current = "";
    setStreamingContent("");
    setToolActivity(null);
    setMessages([]);
    setSessionState("disconnected");

    // Set the new session
    setTaskRunId(newTaskRunId);
    taskRunIdRef.current = newTaskRunId; // Sync ref immediately
    try {
      localStorage.setItem(STORAGE_KEY, newTaskRunId);
    } catch {
      // Ignore storage errors
    }

    // Check if session has a live CLI process
    try {
      const stateResponse = await invoke<CommandResponse>("get_ai_session_state", {
        taskRunId: newTaskRunId,
      });
      const state = stateResponse.data?.state as string | undefined;
      if (state && state !== "not_found" && state !== "closed") {
        setSessionState(state as AiSessionState);
      } else {
        // No live process — show as stopped/historical
        setSessionState("closed");
      }
    } catch {
      setSessionState("closed");
    }

    // Load messages from DB
    try {
      const outputResponse = await invoke<CommandResponse>("get_ai_output", {
        taskRunId: newTaskRunId,
      });
      if (outputResponse.success && outputResponse.data?.output_log) {
        const parsed = parseOutputLog(outputResponse.data.output_log as string);
        setMessages(parsed);
      }
    } catch (e) {
      console.error("[useAiSession] Failed to load session messages:", e);
    }
  }, []);

  const createSession = useCallback(async (taskName?: string) => {
    try {
      const response = await invoke<CommandResponse>("create_ai_session", {
        taskName: taskName || "New Session",
      });

      if (response.success && response.data) {
        const newId = response.data.task_run_id as string;
        setTaskRunId(newId);
        taskRunIdRef.current = newId; // Sync ref immediately so sendMessage works right after
        setSessionState("initializing");
        sessionStateRef.current = "initializing";
        setMessages([]);
        streamingBufferRef.current = "";
        setStreamingContent("");
        try {
          localStorage.setItem(STORAGE_KEY, newId);
        } catch {
          // Ignore storage errors
        }
        return newId;
      }
      return null;
    } catch (e) {
      console.error("[useAiSession] Failed to create session:", e);
      return null;
    }
  }, []);

  const sendMessage = useCallback(async (content: string) => {
    // Use ref to get the latest taskRunId — avoids stale closure when called
    // immediately after createSession (React state update hasn't flushed yet)
    const currentTaskRunId = taskRunIdRef.current;
    if (!currentTaskRunId || sessionStateRef.current === "restoring") return;

    // Add user message to local state immediately
    setMessages((prev) => [
      ...prev,
      { role: "user", content, timestamp: new Date().toISOString() },
    ]);

    // Reset streaming buffer for new AI response
    streamingBufferRef.current = "";
    setStreamingContent("");

    try {
      const response = await invoke<CommandResponse>("send_user_message", {
        taskRunId: currentTaskRunId,
        message: content,
      });

      if (response.data?.state) {
        setSessionState(response.data.state as AiSessionState);
      }
    } catch (e) {
      console.error("[useAiSession] Failed to send message:", e);
    }
  }, []);

  const interrupt = useCallback(async () => {
    if (!taskRunId) return;
    try {
      await invoke<CommandResponse>("interrupt_ai_session", { taskRunId });
    } catch (e) {
      console.error("[useAiSession] Failed to interrupt:", e);
    }
  }, [taskRunId]);

  const close = useCallback(async () => {
    if (!taskRunId) return;
    try {
      await invoke<CommandResponse>("close_ai_session", { taskRunId });
      setSessionState("closed");
      localStorage.removeItem(STORAGE_KEY);
    } catch (e) {
      console.error("[useAiSession] Failed to close:", e);
    }
  }, [taskRunId]);

  const rename = useCallback(
    async (name: string) => {
      if (!taskRunId) return;
      try {
        await invoke<CommandResponse>("rename_ai_session", {
          taskRunId,
          name,
        });
      } catch (e) {
        console.error("[useAiSession] Failed to rename:", e);
      }
    },
    [taskRunId],
  );

  const getOutput = useCallback(async () => {
    if (!taskRunId) return;
    try {
      const response = await invoke<CommandResponse>("get_ai_output", {
        taskRunId,
      });
      if (response.success && response.data?.output_log) {
        const parsed = parseOutputLog(response.data.output_log as string);
        setMessages(parsed);
      }
    } catch (e) {
      console.error("[useAiSession] Failed to get output:", e);
    }
  }, [taskRunId]);

  const generateWorkflow = useCallback(
    async (includeUIBridge: boolean = true, sourceContent?: string) => {
      if (!taskRunId) return null;
      setIsGeneratingWorkflow(true);
      try {
        const response = await invoke<CommandResponse>("generate_workflow_from_session", {
          taskRunId,
          description: "Generate workflow from session conversation",
          includeUiBridge: includeUIBridge,
          sourceContent: sourceContent ?? null,
        });
        setIsGeneratingWorkflow(false);
        if (response.success && response.data?.workflow) {
          return response.data.workflow;
        }
        return null;
      } catch (e) {
        console.error("[useAiSession] Failed to generate workflow:", e);
        setIsGeneratingWorkflow(false);
        return null;
      }
    },
    [taskRunId],
  );

  const resetSession = useCallback(() => {
    setTaskRunId(null);
    setSessionState("disconnected");
    setMessages([]);
    streamingBufferRef.current = "";
    setStreamingContent("");
    setIsGeneratingWorkflow(false);
    setToolActivity(null);
    localStorage.removeItem(STORAGE_KEY);
  }, []);

  return {
    taskRunId,
    sessionState,
    messages,
    streamingContent,
    isGeneratingWorkflow,
    toolActivity,
    createSession,
    switchSession,
    sendMessage,
    interrupt,
    close,
    rename,
    getOutput,
    generateWorkflow,
    resetSession,
  };
}
