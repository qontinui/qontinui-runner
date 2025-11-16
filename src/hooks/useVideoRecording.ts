/**
 * useVideoRecording Hook
 *
 * Manages video recording lifecycle integrated with automation execution.
 * Automatically starts recording when automation begins and stops when it ends.
 *
 * Usage:
 * ```tsx
 * const { startRecordingForExecution, stopRecording, getStatus } = useVideoRecording({
 *   enabled: true,
 *   startDelaySeconds: 2,
 *   maxDurationSeconds: 300,
 *   fps: 15,
 *   quality: 'medium',
 *   codec: 'h264'
 * });
 *
 * // In startExecution callback:
 * await startRecordingForExecution(sessionId);
 *
 * // In stopExecution callback:
 * await stopRecording();
 * ```
 */

import { useCallback, useRef } from 'react';
import { videoRecorder, VideoRecordingConfig } from '../services/VideoRecordingService';

export interface UseVideoRecordingOptions extends VideoRecordingConfig {
  onRecordingStart?: () => void;
  onRecordingStop?: (videoPath: string) => void;
  onRecordingError?: (error: Error) => void;
}

export interface UseVideoRecordingReturn {
  startRecordingForExecution: (sessionId: string) => Promise<void>;
  stopRecording: () => Promise<string | null>;
  getStatus: () => Promise<{ recording: boolean; duration: number; path: string }>;
  isRecordingRef: React.MutableRefObject<boolean>;
}

export function useVideoRecording(options: UseVideoRecordingOptions): UseVideoRecordingReturn {
  const isRecordingRef = useRef(false);

  /**
   * Start recording for an automation execution
   * @param sessionId Unique identifier for this execution session
   */
  const startRecordingForExecution = useCallback(async (sessionId: string) => {
    if (!options.enabled) {
      console.log('[VideoRecording] Recording disabled, skipping');
      return;
    }

    try {
      console.log(`[VideoRecording] Starting recording for session: ${sessionId}`);

      const config: VideoRecordingConfig = {
        enabled: options.enabled,
        startDelaySeconds: options.startDelaySeconds,
        maxDurationSeconds: options.maxDurationSeconds,
        fps: options.fps,
        quality: options.quality,
        codec: options.codec,
      };

      await videoRecorder.startRecording(sessionId, config);
      isRecordingRef.current = true;

      if (options.onRecordingStart) {
        options.onRecordingStart();
      }

      console.log(`[VideoRecording] Recording started successfully with ${options.startDelaySeconds}s delay`);
    } catch (error) {
      console.error('[VideoRecording] Failed to start recording:', error);
      isRecordingRef.current = false;

      if (options.onRecordingError) {
        options.onRecordingError(error as Error);
      }
    }
  }, [options]);

  /**
   * Stop the current recording
   * @returns Path to the saved video file, or null if not recording
   */
  const stopRecording = useCallback(async (): Promise<string | null> => {
    if (!isRecordingRef.current) {
      console.log('[VideoRecording] Not currently recording, skipping stop');
      return null;
    }

    try {
      console.log('[VideoRecording] Stopping recording...');
      const videoPath = await videoRecorder.stopRecording();
      isRecordingRef.current = false;

      if (options.onRecordingStop) {
        options.onRecordingStop(videoPath);
      }

      console.log(`[VideoRecording] Recording stopped. Video saved to: ${videoPath}`);
      return videoPath;
    } catch (error) {
      console.error('[VideoRecording] Failed to stop recording:', error);
      isRecordingRef.current = false;

      if (options.onRecordingError) {
        options.onRecordingError(error as Error);
      }

      return null;
    }
  }, [options]);

  /**
   * Get current recording status
   */
  const getStatus = useCallback(async () => {
    return await videoRecorder.getStatus();
  }, []);

  return {
    startRecordingForExecution,
    stopRecording,
    getStatus,
    isRecordingRef,
  };
}
