/**
 * Managers barrel export
 *
 * Centralized export point for all manager modules.
 */

export { logManager } from "./LogManager";
export type { LogEntry, ImageRecognitionEntry, AiOutputEntry } from "./LogManager";

export { windowManager } from "./WindowManager";

export { actionLogManager } from "./ActionLogManager";

export { eventRouter } from "./EventRouter";

export { setupEventHandlers } from "./EventHandlers";
