/**
 * useConfigurationLoader
 *
 * React hook wrapper for ConfigurationLoaderService.
 * Provides loading operations with success/error callbacks.
 */

import { useCallback } from "react";
import {
  configurationLoader,
  LoadResult,
  ConfigLoadedEventPayload,
} from "../services/ConfigurationLoaderService";

export interface UseConfigurationLoaderOptions {
  /**
   * Called when a load operation succeeds.
   */
  onSuccess?: (result: LoadResult) => void | Promise<void>;
  /**
   * Called when a load operation fails with an error.
   */
  onError?: (result: LoadResult) => void;
}

export interface UseConfigurationLoaderReturn {
  /**
   * Load config from Rust backend (HMR restoration).
   */
  loadFromBackend: () => Promise<LoadResult>;
  /**
   * Load config from a specific file path.
   */
  loadFromPath: (path: string) => Promise<LoadResult>;
  /**
   * Open file dialog and load selected config.
   */
  loadFromDialog: () => Promise<LoadResult>;
  /**
   * Auto-load last config if enabled in settings.
   */
  autoLoad: () => Promise<LoadResult>;
  /**
   * Load the last used configuration (manual trigger).
   */
  loadLastConfiguration: () => Promise<LoadResult>;
  /**
   * Parse config from MCP API event payload.
   */
  parseFromMcpEvent: (payload: ConfigLoadedEventPayload) => LoadResult;
}

/**
 * Hook to perform configuration loading operations.
 * Wraps ConfigurationLoaderService with React callbacks.
 */
export function useConfigurationLoader(
  options: UseConfigurationLoaderOptions = {},
): UseConfigurationLoaderReturn {
  const { onSuccess, onError } = options;

  /**
   * Handle the result of a load operation.
   * Calls onSuccess or onError based on the result.
   */
  const handleResult = useCallback(
    async (result: LoadResult): Promise<LoadResult> => {
      if (result.success) {
        await onSuccess?.(result);
      } else if (result.error) {
        onError?.(result);
      }
      return result;
    },
    [onSuccess, onError],
  );

  const loadFromBackend = useCallback(async (): Promise<LoadResult> => {
    return handleResult(await configurationLoader.loadFromBackend());
  }, [handleResult]);

  const loadFromPath = useCallback(
    async (path: string): Promise<LoadResult> => {
      return handleResult(await configurationLoader.loadFromPath(path));
    },
    [handleResult],
  );

  const loadFromDialog = useCallback(async (): Promise<LoadResult> => {
    return handleResult(await configurationLoader.loadFromDialog());
  }, [handleResult]);

  const autoLoad = useCallback(async (): Promise<LoadResult> => {
    return handleResult(await configurationLoader.autoLoad());
  }, [handleResult]);

  const loadLastConfiguration = useCallback(async (): Promise<LoadResult> => {
    return handleResult(await configurationLoader.loadLastConfiguration());
  }, [handleResult]);

  const parseFromMcpEvent = useCallback(
    (payload: ConfigLoadedEventPayload): LoadResult => {
      const result = configurationLoader.parseFromMcpEvent(payload);
      // Note: This is synchronous, so we need to handle async onSuccess differently
      if (result.success) {
        // Fire and forget for async handlers
        Promise.resolve(onSuccess?.(result));
      } else if (result.error) {
        onError?.(result);
      }
      return result;
    },
    [onSuccess, onError],
  );

  return {
    loadFromBackend,
    loadFromPath,
    loadFromDialog,
    autoLoad,
    loadLastConfiguration,
    parseFromMcpEvent,
  };
}
