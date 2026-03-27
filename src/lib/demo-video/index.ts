export * from "./types";
export { fetchRegisteredElements, planDemoScript } from "./script-planner";
export { executeScript, abortExecution } from "./script-executor";
export type { ExecutionCallbacks } from "./script-executor";
export { generateNarration, generateSrt, generateMarkdownNarration } from "./narration-generator";
export type { NarrationOutput } from "./narration-generator";
