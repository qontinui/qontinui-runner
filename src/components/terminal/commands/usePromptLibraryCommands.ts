/**
 * Dynamic per-prompt slash commands — one registry `CommandAction` per
 * fetched prompt template (`id: "prompt.<slug>"`, `slash: "/<slug>"`).
 *
 * Behavior (plan `2026-07-24-terminal-prompt-library-command` §Entry points):
 * - a prompt with NO parameters runs its `default_action` immediately
 *   (spawn / insert-into-focused / copy);
 * - a prompt WITH parameters opens the `/prompt` modal focused on it so
 *   the operator fills the form.
 *
 * Slash-collision rule (mandatory): `getBySlash` is a first-match linear
 * scan and `register()` replaces only on identical `id` — a colliding
 * slug would register but never resolve. So before registering we check
 * `getBySlash("/<slug>")`; on a collision the slash registration is
 * SKIPPED (the prompt stays reachable via the `/prompt` modal) and one
 * console warning names the shadowing action. Predictability over silent
 * shadowing.
 *
 * Registrations are disposed via the disposer `register()` returns —
 * both on library refresh (the effect re-runs when the prompt list
 * changes; React runs cleanup first) and on unmount.
 */

import { useEffect, useRef } from "react";

import { writeClipboard } from "@/lib/clipboard";

import type { PromptTemplate } from "../promptLibraryApi";
import { renderPromptTemplate } from "../renderPromptTemplate";
import { getBySlash, register } from "./registry";
import type { CommandAction, CommandResult } from "./types";

export interface PromptLibraryCommandsContext {
  /** The fetched library (stable identity between fetches). */
  prompts: PromptTemplate[];
  /** Open the `/prompt` modal, optionally focused on one prompt. */
  openPromptModal: (focusSlug?: string) => void;
  /** Spawn a fresh AI session with the text auto-typed. */
  spawnWithText: (text: string) => void | Promise<void>;
  /** Prefill text (NO trailing `\r`) into the focused session.
   *  Returns false when no session is focused. */
  insertIntoFocused: (text: string) => boolean;
}

function ok(): CommandResult {
  return { ok: true };
}

function fail(code: string, message?: string): CommandResult {
  return { ok: false, code, message };
}

/** Run a parameterless prompt's default action immediately. */
async function runDefaultAction(
  prompt: PromptTemplate,
  ctx: PromptLibraryCommandsContext,
): Promise<CommandResult> {
  // Parameterless — still render so stray `{{placeholders}}` blank out
  // rather than being typed into a session verbatim.
  const text = renderPromptTemplate(prompt.body, {});
  switch (prompt.default_action) {
    case "insert": {
      if (!ctx.insertIntoFocused(text)) {
        return fail("no-focused-session", "no focused terminal session to insert into");
      }
      return ok();
    }
    case "copy": {
      const copied = await writeClipboard(text);
      return copied ? ok() : fail("copy-failed", "could not write to the clipboard");
    }
    case "spawn":
    default:
      await ctx.spawnWithText(text);
      return ok();
  }
}

/**
 * Register one `/<slug>` action per prompt against the module registry.
 * Pure of React so the collision + lifecycle contract is unit-testable;
 * returns a dispose-all that unregisters every action it added.
 *
 * `getCtx` is read at INVOCATION time (the `useCommandAction` ref
 * pattern) so handlers never operate on a stale closure.
 */
export function registerPromptActions(
  prompts: readonly PromptTemplate[],
  getCtx: () => PromptLibraryCommandsContext,
): () => void {
  const disposers: Array<() => void> = [];

  for (const prompt of prompts) {
    const slash = `/${prompt.name}`;
    const shadowing = getBySlash(slash);
    if (shadowing) {
      // Mandatory collision rule: skip the slash registration; the
      // prompt stays reachable via the /prompt modal.
      console.warn(
        `prompt-library: skipping slash registration for "${slash}" — ` +
          `already taken by action "${shadowing.id}" (${shadowing.slash}); ` +
          `the prompt remains available in the /prompt modal`,
      );
      continue;
    }

    const action: CommandAction = {
      id: `prompt.${prompt.name}`,
      slash,
      label: prompt.title,
      description:
        (prompt.description || `Run the "${prompt.title}" prompt template.`) +
        (prompt.parameters.length > 0
          ? " Opens the prompt form."
          : ` Runs immediately (${prompt.default_action}).`),
      paramSchema: {},
      handler: async (): Promise<CommandResult> => {
        const current = getCtx();
        const live = current.prompts.find((p) => p.name === prompt.name) ?? prompt;
        if (live.parameters.length > 0) {
          current.openPromptModal(live.name);
          return ok();
        }
        return runDefaultAction(live, current);
      },
    };
    disposers.push(register(action));
  }

  return () => {
    for (const dispose of disposers) dispose();
  };
}

export function usePromptLibraryCommands(ctx: PromptLibraryCommandsContext): void {
  // Latest-context ref (the `useCommandAction` pattern): handlers close
  // over the ref so registrations survive re-renders without re-running
  // the effect, which is keyed on the prompt list only.
  const ctxRef = useRef(ctx);
  ctxRef.current = ctx;

  const { prompts } = ctx;

  useEffect(() => {
    // React runs this cleanup before re-running the effect, so a library
    // refresh disposes the old registrations before adding the new set.
    return registerPromptActions(prompts, () => ctxRef.current);
  }, [prompts]);
}
