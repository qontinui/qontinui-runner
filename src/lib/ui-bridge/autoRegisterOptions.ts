import type { AutoRegisterProviderProps } from "@qontinui/ui-bridge";

/**
 * How the runner mounts the SDK's DOM auto-registration (`App.tsx`'s
 * `<AutoRegisterProvider {...RUNNER_AUTO_REGISTER_OPTIONS}>`).
 *
 * A module of its own so a test can mount the SDK exactly the way the runner
 * does and observe what the runner's registry then serves — the element
 * `actions` the runner's pre-IPC gate (`is_action_advertised`,
 * `src-tauri/src/mcp/ui_bridge/elements.rs`) refuses against, among them.
 *
 * - `enabled` unconditionally: gating this on `import.meta.env.DEV` once
 *   disabled the registry in every exe-mode build (the temp runner and the
 *   embedded primary), so element discovery returned near-empty results.
 * - `prefer-existing`: an author-stamped `data-ui-bridge-id` wins over the
 *   SDK's text-derived id.
 * - `debounceMs: 100`: the MutationObserver scan's debounce. The runner is a
 *   power-user tool with a tight automation loop, so the registry has to work
 *   in every shipped build, not just dev.
 * - `[data-no-register]` opts an element out individually.
 */
export const RUNNER_AUTO_REGISTER_OPTIONS = {
  enabled: true,
  idStrategy: "prefer-existing",
  debounceMs: 100,
  excludeSelectors: ["[data-no-register]"],
  contentDiscovery: { enabled: true, maxContentElements: 200 },
} satisfies Omit<AutoRegisterProviderProps, "children">;
