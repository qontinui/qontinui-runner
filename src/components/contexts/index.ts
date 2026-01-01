/**
 * Contexts Components
 *
 * Components for managing and selecting AI contexts.
 */

// Selection components (for AI task integration)
export { ContextSelector } from "./ContextSelector";
export type { ContextSelectorProps } from "./ContextSelector";

// Management components (for listing and editing)
export { ContextList } from "./ContextList";
export { ContextCard } from "./ContextCard";
export { ContextEditor } from "./ContextEditor";
export { ContextFilters } from "./ContextFilters";

// Hooks
export { useContexts, groupContextsByScope } from "./hooks/useContexts";

// Default export (ContextList for management UI)
export { ContextList as default } from "./ContextList";
