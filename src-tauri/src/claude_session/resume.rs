//! Chat session resume support.
//!
//! Parses conversation history from the output_log (persisted [USER_MESSAGE],
//! [AI_RESPONSE], and [SYSTEM_NOTE] blocks) and builds a replay prompt for
//! resuming interrupted sessions.

/// Role of a conversation turn.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnRole {
    User,
    Assistant,
    /// System-injected note (e.g., workflow generation results, idle markers).
    SystemNote,
}

/// A single turn in a conversation.
#[derive(Debug, Clone)]
pub struct ConversationTurn {
    pub role: TurnRole,
    pub content: String,
}

/// Parse [USER_MESSAGE], [AI_RESPONSE], and [SYSTEM_NOTE] blocks from an output_log string.
///
/// Returns turns in the order they appear. Ignores any text outside of
/// recognized marker blocks (e.g., [SESSION_START] markers, stray text).
pub fn parse_conversation(output_log: &str) -> Vec<ConversationTurn> {
    let mut turns = Vec::new();
    let mut remaining = output_log;

    while !remaining.is_empty() {
        // Find the next marker — whichever comes first
        let user_pos = remaining.find("[USER_MESSAGE]");
        let ai_pos = remaining.find("[AI_RESPONSE]");
        let note_pos = remaining.find("[SYSTEM_NOTE]");

        // Find the earliest marker position
        let earliest = [user_pos, ai_pos, note_pos].iter().filter_map(|p| *p).min();

        match earliest {
            None => break,
            Some(pos) => {
                if user_pos == Some(pos) {
                    if let Some(turn) =
                        extract_block(remaining, "[USER_MESSAGE]", "[/USER_MESSAGE]")
                    {
                        turns.push(ConversationTurn {
                            role: TurnRole::User,
                            content: turn.content,
                        });
                        remaining = turn.rest;
                    } else {
                        break;
                    }
                } else if ai_pos == Some(pos) {
                    if let Some(turn) = extract_block(remaining, "[AI_RESPONSE]", "[/AI_RESPONSE]")
                    {
                        turns.push(ConversationTurn {
                            role: TurnRole::Assistant,
                            content: turn.content,
                        });
                        remaining = turn.rest;
                    } else {
                        break;
                    }
                } else if note_pos == Some(pos) {
                    if let Some(turn) = extract_block(remaining, "[SYSTEM_NOTE]", "[/SYSTEM_NOTE]")
                    {
                        turns.push(ConversationTurn {
                            role: TurnRole::SystemNote,
                            content: turn.content,
                        });
                        remaining = turn.rest;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
    }

    turns
}

/// Result of extracting a block from the string.
struct ExtractedBlock<'a> {
    content: String,
    rest: &'a str,
}

/// Extract content between open_tag and close_tag, returning the content
/// and the remainder of the string after the close tag.
fn extract_block<'a>(
    input: &'a str,
    open_tag: &str,
    close_tag: &str,
) -> Option<ExtractedBlock<'a>> {
    let start = input.find(open_tag)?;
    let content_start = start + open_tag.len();
    let close_pos = input[content_start..].find(close_tag)?;
    let content = input[content_start..content_start + close_pos]
        .trim()
        .to_string();
    let rest_start = content_start + close_pos + close_tag.len();
    Some(ExtractedBlock {
        content,
        rest: &input[rest_start..],
    })
}

/// Default maximum character limit for the replay prompt context.
const DEFAULT_MAX_CHARS: usize = 100_000;

/// Build a replay prompt from conversation history.
///
/// Formats the conversation as XML-tagged context with instructions for Claude
/// to continue naturally. Truncates oldest turns (keeping the first user message)
/// if the total exceeds `max_chars`.
pub fn build_replay_prompt(turns: &[ConversationTurn], max_chars: Option<usize>) -> String {
    if turns.is_empty() {
        return String::new();
    }

    let limit = max_chars.unwrap_or(DEFAULT_MAX_CHARS);

    // Format all turns
    let formatted_turns: Vec<String> = turns
        .iter()
        .map(|turn| {
            let tag = match turn.role {
                TurnRole::User => "user",
                TurnRole::Assistant => "assistant",
                TurnRole::SystemNote => "system_note",
            };
            format!("<{}>\n{}\n</{}>", tag, turn.content, tag)
        })
        .collect();

    // If everything fits, use all turns
    let full_history = formatted_turns.join("\n");
    if full_history.len() <= limit {
        return format_replay_prompt(&full_history, turns);
    }

    // Truncate: always keep the first turn, then as many recent turns as fit
    let first_turn = &formatted_turns[0];
    let mut budget = limit.saturating_sub(first_turn.len() + 50); // 50 chars for separator
    let mut included_from_end = Vec::new();

    for turn in formatted_turns[1..].iter().rev() {
        if turn.len() + 1 > budget {
            break;
        }
        budget -= turn.len() + 1; // +1 for newline
        included_from_end.push(turn.as_str());
    }
    included_from_end.reverse();

    let truncated_history = if included_from_end.is_empty() {
        first_turn.clone()
    } else {
        format!(
            "{}\n\n[... earlier conversation truncated ...]\n\n{}",
            first_turn,
            included_from_end.join("\n")
        )
    };

    format_replay_prompt(&truncated_history, turns)
}

/// Format the final replay prompt with instructions.
fn format_replay_prompt(history: &str, turns: &[ConversationTurn]) -> String {
    let last_role_instruction = match turns.last().map(|t| &t.role) {
        Some(TurnRole::User) => "The last message was from the user — respond to it.",
        Some(TurnRole::SystemNote) => {
            "The last entry is a system note indicating the conversation is idle. \
             Do NOT produce any output. Do NOT ask questions, offer next steps, or \
             summarize anything. Simply wait silently for the user to send a new message."
        }
        Some(TurnRole::Assistant) | None => {
            "The last message was from you. Wait for the user's next message."
        }
    };

    format!(
        "You are continuing a conversation that was interrupted by a system restart. \
Below is the conversation history. Continue as if the conversation never stopped. \
Do NOT repeat or summarize the previous conversation — just pick up where you left off.\n\
{}\n\n\
<conversation_history>\n\
{}\n\
</conversation_history>",
        last_role_instruction, history
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty() {
        let turns = parse_conversation("");
        assert!(turns.is_empty());
    }

    #[test]
    fn test_parse_single_user_message() {
        let log = "\n[USER_MESSAGE]\nHello world\n[/USER_MESSAGE]\n";
        let turns = parse_conversation(log);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[0].content, "Hello world");
    }

    #[test]
    fn test_parse_conversation_roundtrip() {
        let log = "\n[USER_MESSAGE]\nWhat is Rust?\n[/USER_MESSAGE]\n\
                   \n[AI_RESPONSE]\nRust is a systems programming language.\n[/AI_RESPONSE]\n\
                   \n[USER_MESSAGE]\nTell me more\n[/USER_MESSAGE]\n\
                   \n[AI_RESPONSE]\nRust focuses on safety and performance.\n[/AI_RESPONSE]\n";

        let turns = parse_conversation(log);
        assert_eq!(turns.len(), 4);
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[0].content, "What is Rust?");
        assert_eq!(turns[1].role, TurnRole::Assistant);
        assert_eq!(turns[1].content, "Rust is a systems programming language.");
        assert_eq!(turns[2].role, TurnRole::User);
        assert_eq!(turns[2].content, "Tell me more");
        assert_eq!(turns[3].role, TurnRole::Assistant);
        assert_eq!(turns[3].content, "Rust focuses on safety and performance.");
    }

    #[test]
    fn test_build_replay_prompt_basic() {
        let turns = vec![
            ConversationTurn {
                role: TurnRole::User,
                content: "Hello".to_string(),
            },
            ConversationTurn {
                role: TurnRole::Assistant,
                content: "Hi there!".to_string(),
            },
        ];

        let prompt = build_replay_prompt(&turns, None);
        assert!(prompt.contains("<conversation_history>"));
        assert!(prompt.contains("<user>\nHello\n</user>"));
        assert!(prompt.contains("<assistant>\nHi there!\n</assistant>"));
        assert!(prompt.contains("Wait for the user's next message"));
    }

    #[test]
    fn test_build_replay_prompt_last_user() {
        let turns = vec![
            ConversationTurn {
                role: TurnRole::User,
                content: "Hello".to_string(),
            },
            ConversationTurn {
                role: TurnRole::Assistant,
                content: "Hi!".to_string(),
            },
            ConversationTurn {
                role: TurnRole::User,
                content: "What time is it?".to_string(),
            },
        ];

        let prompt = build_replay_prompt(&turns, None);
        assert!(prompt.contains("respond to it"));
    }

    #[test]
    fn test_build_replay_prompt_empty() {
        let prompt = build_replay_prompt(&[], None);
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_parse_ignores_stray_text() {
        let log = "Some stray text here\n[SESSION_START:1]\n\
                   [USER_MESSAGE]\nHello\n[/USER_MESSAGE]\n\
                   random noise\n\
                   [AI_RESPONSE]\nWorld\n[/AI_RESPONSE]\n";

        let turns = parse_conversation(log);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].content, "Hello");
        assert_eq!(turns[1].content, "World");
    }

    #[test]
    fn test_parse_system_notes() {
        let log = "[USER_MESSAGE]\nCreate a workflow\n[/USER_MESSAGE]\n\
                   [AI_RESPONSE]\nHere's the workflow plan...\n[/AI_RESPONSE]\n\
                   [SYSTEM_NOTE]\n[SYSTEM NOTE: Workflow generated. Conversation is idle.]\n[/SYSTEM_NOTE]\n";

        let turns = parse_conversation(log);
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].role, TurnRole::User);
        assert_eq!(turns[1].role, TurnRole::Assistant);
        assert_eq!(turns[2].role, TurnRole::SystemNote);
        assert!(turns[2].content.contains("idle"));
    }

    #[test]
    fn test_replay_prompt_system_note_idle() {
        let turns = vec![
            ConversationTurn {
                role: TurnRole::User,
                content: "Create a workflow".to_string(),
            },
            ConversationTurn {
                role: TurnRole::Assistant,
                content: "Here's the plan...".to_string(),
            },
            ConversationTurn {
                role: TurnRole::SystemNote,
                content: "Workflow generated. Conversation is idle.".to_string(),
            },
        ];

        let prompt = build_replay_prompt(&turns, None);
        assert!(prompt.contains("<system_note>"));
        assert!(prompt.contains("Do NOT produce any output"));
        assert!(prompt.contains("wait silently"));
    }
}
