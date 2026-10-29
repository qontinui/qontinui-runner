/**
 * The ONE composition of a pane's label (plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
 * Phase 4 / D3).
 *
 * A pane carries two titles from two sources:
 *
 * - **Identity**: the bound conversation's name and id. The primary line is
 *   `<name> · <id8>`. It is `(unnamed) · <id8>` when the provider has no name,
 *   and `no conversation bound` when no id is bound.
 * - **Activity**: the OSC-0/2 title the CLI emits for what it is doing right
 *   now, e.g. the description of the last subagent it launched. That string
 *   only ever appears as the secondary line.
 *
 * Until D3 the OSC relay overwrote `tab.title` with the activity title. On
 * 2026-10-27 that left a live lead pane titled "Fix Phase 5b round 11", with
 * nothing on screen naming its conversation, and the operator opened a second
 * copy of it.
 *
 * TS twin of `src-tauri/src/session/pane_label.rs` `conversation_label`. Both
 * are pinned to `__fixtures__/pane-label.json`.
 */

/** The name shown when a conversation is bound but the provider has none. */
export const UNNAMED = "(unnamed)";

/** The whole label when no conversation id is bound to the pane. */
export const NO_CONVERSATION_BOUND = "no conversation bound";

/** Separator between the name and the short id. */
export const SEPARATOR = " · ";

/** How many leading characters of the conversation id the label shows. */
export const ID_PREFIX_CHARS = 8;

function clean(s: string | null | undefined): string | undefined {
  if (typeof s !== "string") return undefined;
  const t = s.trim();
  return t.length > 0 ? t : undefined;
}

/** First {@link ID_PREFIX_CHARS} characters (code points) of a trimmed id. */
export function id8(claudeSessionId: string): string {
  return Array.from(claudeSessionId.trim()).slice(0, ID_PREFIX_CHARS).join("");
}

/**
 * The label's two halves, so a renderer can let the name truncate while the
 * `· <id8>` suffix stays visible. `null` when no id is bound.
 */
export function conversationLabelParts(
  name: string | null | undefined,
  claudeSessionId: string | null | undefined,
): { name: string; suffix: string } | null {
  const id = clean(claudeSessionId);
  if (!id) return null;
  return { name: clean(name) ?? UNNAMED, suffix: `${SEPARATOR}${id8(id)}` };
}

/** `<name> · <id8>`, `(unnamed) · <id8>`, or `no conversation bound`. */
export function conversationLabel(
  name: string | null | undefined,
  claudeSessionId: string | null | undefined,
): string {
  const parts = conversationLabelParts(name, claudeSessionId);
  return parts ? `${parts.name}${parts.suffix}` : NO_CONVERSATION_BOUND;
}

export interface PaneLabelInput {
  /** The conversation's name (live registry name, or the record's title). */
  conversationName?: string | null;
  /** The conversation id bound to the pane. Absent for a plain shell. */
  claudeSessionId?: string | null;
  /** The child's OSC-0/2 current-activity title. */
  activityTitle?: string | null;
  /**
   * The pane's shell title, already resolved for display. It is the primary
   * line of a pane that hosts no conversation.
   */
  shellTitle: string;
}

export interface PaneLabel {
  /** The primary line's full text. */
  primary: string;
  /** Present when the primary line names a conversation: the truncating name. */
  name?: string;
  /** Present when the primary line names a conversation: ` · <id8>`, never truncated. */
  idSuffix?: string;
  /**
   * The secondary line: the activity title, for a conversation pane only. It
   * is omitted when blank or identical to the primary line. A plain shell
   * shows its OSC title as the primary line through `shellTitle`, so it has
   * no secondary line.
   */
  secondary?: string;
}

/**
 * Compose every rendered pane label.
 *
 * - A pane bound to a conversation leads with `<name> · <id8>`, and the
 *   activity title is its secondary line.
 * - A plain shell pane, with no conversation bound, keeps its shell title as
 *   the primary line.
 */
export function paneLabel(input: PaneLabelInput): PaneLabel {
  const parts = conversationLabelParts(input.conversationName, input.claudeSessionId);
  if (!parts) {
    return { primary: input.shellTitle };
  }
  const primary = `${parts.name}${parts.suffix}`;
  const activity = clean(input.activityTitle);
  return {
    primary,
    name: parts.name,
    idSuffix: parts.suffix,
    secondary: activity && activity !== primary ? activity : undefined,
  };
}
