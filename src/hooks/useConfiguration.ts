/**
 * useConfiguration
 *
 * Re-export from the refactored configuration hooks module.
 * This file exists for backward compatibility.
 *
 * @see ./configuration/ for the refactored implementation
 */

export { useConfiguration } from "./configuration";
export type {
  Config,
  ConfigImage,
  ConfigState,
  StateImage,
  Workflow,
  UseConfigurationOptions,
  UseConfigurationReturn,
} from "./configuration";
