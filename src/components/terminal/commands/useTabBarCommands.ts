/**
 * Slash commands for the terminal page TAB BAR (the strip of pages above the
 * grid): `/close-empty-tabs` and `/reorganize-tabs`. They are the command-bar
 * twins of the broom and shuffle buttons in `TerminalPageTabBar`, and share
 * their underlying closures so the two surfaces cannot drift.
 *
 * Registered from `App.tsx` rather than `useTerminalCommands`, because the
 * page roster (`useTerminalPages`) and the reorganize modal live in `App`, not
 * in `TerminalPage`. The registry is module-global, so that is equivalent.
 */

import { useCommandAction } from "./useCommandAction";
import { effect, ok, stateEffect, fail, type EffectReport } from "./verdict";
import type { CommandAction, CommandResult } from "./types";
import { describeThrown } from "@/lib/utils";

export interface TabBarCommandsContext {
  /** Number of tabs currently in the strip (visible pages). */
  pageCount: number;
  /** Close every page that hosts no terminal. Resolves to how many were closed. */
  closeEmptyPages: () => Promise<number>;
  /** Open the AI reorganize-by-topic modal. */
  openReorganize: () => void;
  /** Whether that modal is already open. */
  reorganizeOpen: boolean;
}

/** Exported (as builders, not just the hook) so the handlers are testable without React. */
export function closeEmptyTabsAction(ctx: TabBarCommandsContext): CommandAction {
  return {
    id: "terminal.close-empty-tabs",
    slash: "/close-empty-tabs",
    aliases: ["/close-empty"],
    label: "Close empty tabs",
    description:
      "Close every terminal page (tab) that has no terminals open. Same as the broom " +
      "icon in the tab bar. Re-checks each tab right before closing it and closes " +
      "nothing if tab occupancy cannot be read.",
    paramSchema: {},
    patterns: [/^close\s+empty\s+(?:tabs|pages)$/i],
    destructive: true,
    handler: async (): Promise<CommandResult<EffectReport>> => {
      if (ctx.pageCount < 2) return fail("no-tabs", "Only one tab; nothing to close");
      try {
        const closed = await ctx.closeEmptyPages();
        return ok(effect("closed", "empty tab", closed));
      } catch (err) {
        return fail("close-empty-failed", describeThrown(err, "close empty tabs"));
      }
    },
  } as CommandAction;
}

export function reorganizeTabsAction(ctx: TabBarCommandsContext): CommandAction {
  return {
    id: "terminal.reorganize-tabs",
    slash: "/reorganize-tabs",
    aliases: ["/reorganize", "/reorg"],
    label: "Reorganize tabs by topic",
    description:
      "Open the AI reorganize dialog, which groups sessions into tabs by topic. " +
      "Same as the shuffle icon in the tab bar. Needs at least two tabs.",
    paramSchema: {},
    patterns: [/^reorgani[sz]e(?:\s+(?:tabs|pages))?$/i],
    handler: async (): Promise<CommandResult<EffectReport>> => {
      if (ctx.pageCount < 2) return fail("no-tabs", "Needs at least two tabs to reorganize");
      const wasOpen = ctx.reorganizeOpen;
      ctx.openReorganize();
      return ok(stateEffect("opened", "the reorganize dialog", !wasOpen));
    },
  } as CommandAction;
}

export function useTabBarCommands(ctx: TabBarCommandsContext): void {
  useCommandAction(closeEmptyTabsAction(ctx));
  useCommandAction(reorganizeTabsAction(ctx));
}
