//! Permission-prompt capture — classify every permission prompt the runner
//! hears about into a stable class, redact it, and count it per class per
//! hour (plan
//! `2026-10-14-permission-prompts-are-never-recorded-so-guard-rules-grow-on-anecdote`,
//! Phase 2, D3/D4/D6).
//!
//! The intake is the EXISTING one: the Claude Code `Notification` hook's
//! `POST /sessions/{id}/notification` landing pad
//! ([`crate::mcp::sessions`]) hands its raw body to [`observe_notification`]
//! before the operator-touch emit runs, independently of it. There is no new
//! hook script and no `/hooks/` route.
//!
//! Three pieces, each usable alone:
//!
//! - [`classify`] — the D3 pure function: prompt text (+ optional tool name and
//!   command) → a readable **class key** and a **dossier slug**
//!   `ppc-<hash8(class key)>`. The slug is deliberately NOT the readable key:
//!   the findings steward folds any slug sharing ≥ 60 % of its hyphen tokens
//!   with an existing head into that head, so `permission-prompt-bash-rm` and
//!   `permission-prompt-bash-git` would collapse. Also served verbatim by
//!   `POST /permission-prompts/classify` so the `/permission-prompts record`
//!   command classifies a pasted prompt identically.
//!
//!   **The command-token rule** (the hook-side key, since Phase 0 found no
//!   reason text in either hook payload): the command is split into
//!   command-position segments at unquoted `;` `&&` `||` `|` `&` `(` `)` `{`
//!   `}` and newlines; each segment's command word skips shell keywords and
//!   prefixes (`if then else elif fi for while until do done case esac ! time
//!   { } ( ) env sudo command builtin exec nohup`) and `VAR=value`
//!   assignments, and is reduced to its basename. If any command word is a
//!   removal command (`rm`, `rmdir`) — what Claude Code's built-in
//!   dangerous-removal check fires on — the first such word is the token, so
//!   `if git show …; then rm -rf "$X/…"; fi` keys as `permission-prompt-bash-rm`,
//!   exactly as its pasted "Dangerous rm operation …" reason does. Otherwise
//!   the token is the FIRST command word (`cd /x && npm test` → `bash-cd`):
//!   coarse, but stable, because no payload says which sub-command prompted.
//! - [`redact_example`] — D6: the ONE redactor
//!   ([`crate::session::redact::redact_secrets`]), then `VAR=value`
//!   assignment values stripped, the command cut to
//!   [`COMMAND_EXAMPLE_MAX_CHARS`] chars, and `cwd` reduced to its last two
//!   path components.
//! - [`Aggregator`] — D4: in-memory, keyed by (dossier slug, UTC hour bucket).
//!   [`snapshot`] and [`drain_completed_hours`] are the read side Phase 3's
//!   hourly coord flush consumes. **Nothing here writes to coord** (Phase 3).
//!   Unflushed counts are lost when the runner exits — accepted by D4.
//!
//! **Observe, never decide (D1).** Nothing in this module can change what the
//! operator sees; every entry point is fail-open and never panics.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, TimeZone, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Prefix every readable class key carries.
pub const CLASS_KEY_PREFIX: &str = "permission-prompt";
/// Prefix every dossier slug carries (`ppc-<hash8>`).
pub const DOSSIER_SLUG_PREFIX: &str = "ppc-";
/// How many redacted examples one (class, hour) bucket keeps (D4).
pub const MAX_EXAMPLES_PER_BUCKET: usize = 3;
/// The command excerpt cap (D6).
pub const COMMAND_EXAMPLE_MAX_CHARS: usize = 300;
/// The reason-text cap. The reason is recorded verbatim (after redaction), but
/// a pathological payload must not grow the aggregator without bound.
pub const REASON_MAX_CHARS: usize = 500;
/// How many live buckets the aggregator holds. At the bound a NEW bucket
/// evicts the oldest-hour bucket, and an observation older than every held
/// bucket is refused; both losses are counted in [`Aggregator::dropped`].
pub const MAX_BUCKETS: usize = 512;
/// A `Notification` arriving within this window of the last prompt signal on
/// the same terminal — a counted `PermissionRequest`, or a counted or
/// suppressed `Notification` — is treated as the same dialog and not counted
/// again. The window slides with each suppressed `Notification`. Phase 0 saw
/// a `Notification` follow a `PermissionRequest` ~6 s later; whether Claude
/// Code REPEATS the `Notification` while a dialog stays open was not
/// measured, so the window guards against it rather than relying on it.
pub const DEDUP_WINDOW_SECS: i64 = 60;
/// Cap on a tool name or command word inside a class key.
pub const MAX_KEY_TOKEN_CHARS: usize = 48;
/// Cap on a stored cwd excerpt (after reduction to two components).
pub const CWD_EXAMPLE_MAX_CHARS: usize = 200;
/// Cap on the prompt text and the command `classify` looks at — a pathological
/// payload must not cost unbounded regex work.
pub const MAX_CLASSIFY_INPUT_BYTES: usize = 16 * 1024;
/// How many distinct permission modes one bucket keeps, and their length cap.
pub const MAX_PERMISSION_MODES: usize = 8;
pub const MAX_PERMISSION_MODE_CHARS: usize = 32;
/// How many readable words of the normalized reason go into the class key.
const MAX_KEY_WORDS: usize = 6;

// =============================================================================
// D3 — classification
// =============================================================================

/// The output of [`classify`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PromptClass {
    /// Readable, stable class key, e.g.
    /// `permission-prompt-dangerous-rm-operation-on-possibly-empty-1a2b3c`
    /// (reason-keyed) or `permission-prompt-bash-rm` (fallback).
    pub class_key: String,
    /// `ppc-<hash8 of class_key>` — the `artifact_refs.dossier_slug` (D5).
    pub dossier_slug: String,
    /// The reason paragraph as found in the text (NOT redacted here — the
    /// aggregator redacts before storing). `None` when the text carried only
    /// the stock phrase.
    pub reason: Option<String>,
    /// The tool the prompt was for, when known (lowercase, sanitized).
    pub tool: Option<String>,
}

/// Lines Claude Code renders around every prompt that say nothing about WHY it
/// prompted. Compared lowercase, against the trimmed line's start.
const STOCK_LINE_PREFIXES: &[&str] = &[
    "claude needs your permission",
    "do you want to proceed",
    "do you want to make this edit",
    "do you want to",
    "esc to cancel",
    "bash command",
    "tool use",
];

/// A numbered choice line (`❯ 1. Yes`, `2. No, and tell Claude…`).
static CHOICE_LINE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[\s❯>›»]*\d+\.\s").expect("choice-line regex is a valid literal"));

/// A sentence-shaped line: a capitalized word followed by at least two more
/// words. A shell command rarely starts this way, so this is what separates
/// the reason from a pasted command preview.
static SENTENCE_LINE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^[A-Z][a-z]+(\s+\S+){2,}").expect("sentence-line regex is a valid literal")
});

/// A built-in command-safety reason that names its command:
/// `Dangerous rm operation on possibly-empty variable path: …`.
static DANGEROUS_OP_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^\s*dangerous\s+([A-Za-z0-9_.\-]+)\s+operation\b")
        .expect("dangerous-op regex is a valid literal")
});

/// The tool a "Dangerous <cmd> operation" reason belongs to when the caller
/// names none — the built-in command-safety checks guard `Bash`.
const DEFAULT_SAFETY_TOOL: &str = "bash";

/// `Claude needs your permission to use <Tool>`.
static STOCK_TOOL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)permission to use\s+([A-Za-z0-9_:.\-]+)")
        .expect("stock-tool regex is a valid literal")
});

// Normalization placeholders, applied in order (D3).
static BACKTICK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"`[^`]*`").expect("backtick regex is a valid literal"));
static DQUOTE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#""[^"]*""#).expect("double-quote regex is a valid literal"));
/// A single-quoted fragment only where a quote OPENS (start, whitespace or
/// punctuation before it), so an apostrophe in `don't … won't` is not one.
static SQUOTE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(^|[\s(=:,])'[^']*'").expect("single-quote regex is a valid literal")
});
static CMDSUB_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\$\([^)]*\)").expect("cmdsub regex is a valid literal"));
static VAR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\$\{[^}]*\}|\$[A-Za-z_][A-Za-z0-9_]*|\$[0-9@#?*!$-]")
        .expect("variable regex is a valid literal")
});
static PATH_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[^\s,;()<>]*[\\/][^\s,;()<>]*").expect("path regex is valid"));
static NUMBER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b\d+\b").expect("number regex is a valid literal"));
static SPACE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\s+").expect("whitespace regex is a valid literal"));
static PLACEHOLDER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"<(q|var|path|n)>").expect("placeholder regex is valid"));

/// Hex SHA-256 of `s`, cut to `n` hex chars. SHA-256 rather than
/// `std::collections::hash_map::DefaultHasher`, which is NOT stable across
/// builds — a class key must hash identically on every runner and in the
/// `/permission-prompts record` path.
fn hash_hex(s: &str, n: usize) -> String {
    let digest = Sha256::digest(s.as_bytes());
    let mut hex = hex::encode(digest);
    hex.truncate(n);
    hex
}

/// Lowercase and squeeze to `[a-z0-9]` runs joined by single hyphens.
fn sanitize_token(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    // ASCII-only by construction, so a char cut is a byte cut.
    let mut out: String = out.chars().take(MAX_KEY_TOKEN_CHARS).collect();
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Normalize reason text: quoted fragments, variables, paths and numbers
/// become placeholders; whitespace collapses; case folds. Two prompts of one
/// class that differ only in WHICH variable or path they name normalize equal.
pub fn normalize_reason(text: &str) -> String {
    let s = BACKTICK_RE.replace_all(text, "<q>");
    let s = DQUOTE_RE.replace_all(&s, "<q>");
    let s = SQUOTE_RE.replace_all(&s, "$1<q>");
    let s = CMDSUB_RE.replace_all(&s, "<var>");
    let s = VAR_RE.replace_all(&s, "<var>");
    let s = PATH_RE.replace_all(&s, "<path>");
    let s = NUMBER_RE.replace_all(&s, "<n>");
    let s = SPACE_RE.replace_all(&s, " ");
    s.trim()
        .trim_end_matches(['.', ',', ';', ':'])
        .to_lowercase()
}

fn is_stock_line(line: &str) -> bool {
    let lower = line.trim().to_lowercase();
    STOCK_LINE_PREFIXES.iter().any(|p| lower.starts_with(p)) || CHOICE_LINE_RE.is_match(line)
}

/// Find the reason paragraph in prompt text: the first sentence-shaped,
/// non-stock line (plus the lines that follow it up to a blank line). Lines
/// that echo the command, or start with an ellipsis (a truncated command
/// preview), are skipped. `None` when only stock phrasing is present.
fn extract_reason(text: &str, command: Option<&str>) -> Option<String> {
    let command = command.map(str::trim).filter(|c| !c.is_empty());
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|raw| {
        let line = raw.trim();
        !line.is_empty()
            && !is_stock_line(line)
            && !line.starts_with('…')
            && !line.starts_with("...")
            && !command.is_some_and(|c| line.contains(c))
            && SENTENCE_LINE_RE.is_match(line)
    })?;
    let mut para = Vec::new();
    for raw in &lines[start..] {
        let line = raw.trim();
        if line.is_empty() || is_stock_line(line) {
            break;
        }
        para.push(line);
    }
    Some(para.join("\n"))
}

/// Shell words that open or close a construct, or prefix the real program,
/// and so are never the command a prompt is about.
const SHELL_KEYWORDS_AND_PREFIXES: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac", "!",
    "time", "{", "}", "(", ")", "env", "sudo", "command", "builtin", "exec", "nohup",
];

/// Removal commands. Claude Code's built-in dangerous-removal check is the one
/// that fires for these, and its "Dangerous <cmd> operation" reason names one
/// of them, so a compound command containing one is keyed on it.
const REMOVAL_COMMANDS: &[&str] = &["rm", "rmdir"];

fn is_assignment(tok: &str) -> bool {
    match tok.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Split a command into its command-position segments: a new segment starts
/// at the beginning and after every unquoted `;`, `&&`, `||`, `|`, `&`, `(`,
/// `)`, `{`, `}` and newline. Quote-aware (single and double quotes), so a
/// separator inside a quoted argument does not split. A lexer for keying,
/// not a shell parser.
fn command_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                cur.push(c);
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                ';' | '&' | '|' | '(' | ')' | '{' | '}' | '\n' => {
                    segments.push(std::mem::take(&mut cur));
                }
                _ => cur.push(c),
            },
        }
    }
    segments.push(cur);
    segments
}

/// The command word of one segment: leading shell keywords, prefixes
/// (`sudo`, `env`, …) and `VAR=value` assignments skipped, path stripped to
/// its basename, sanitized.
fn segment_command_word(segment: &str) -> Option<String> {
    segment
        .split_whitespace()
        .map(|tok| tok.trim_matches(['"', '\'']))
        .find(|tok| {
            !tok.is_empty() && !SHELL_KEYWORDS_AND_PREFIXES.contains(tok) && !is_assignment(tok)
        })
        .map(|tok| sanitize_token(tok.rsplit(['/', '\\']).next().unwrap_or(tok)))
        .filter(|t| !t.is_empty())
}

/// The command token a prompt is keyed on (D3 fallback key). Rule:
///
/// 1. If any command-position word is a removal command (`rm`, `rmdir`), the
///    first such word — so `if git show …; then rm -rf "$X/…"; fi` keys as
///    `rm`, the same key a pasted "Dangerous rm operation" reason gets.
/// 2. Otherwise the first command-position word that is not a shell keyword,
///    prefix or assignment — `cd /x && npm test` keys as `cd`. Coarse but
///    stable: the hook payload does not say which sub-command prompted.
fn first_command_token(command: &str) -> Option<String> {
    let words: Vec<String> = command_segments(command)
        .iter()
        .filter_map(|s| segment_command_word(s))
        .collect();
    words
        .iter()
        .find(|w| REMOVAL_COMMANDS.contains(&w.as_str()))
        .or_else(|| words.first())
        .cloned()
}

/// D3: classify one prompt.
///
/// - `text`: whatever prose the prompt carried — a `Notification` `message`,
///   or text an operator pasted out of a blocked pane.
/// - `tool_name` / `command`: from a `PermissionRequest` payload or the
///   `record` verb, when available.
///
/// A "Dangerous <cmd> operation …" reason (a built-in command-safety check) →
/// `permission-prompt-<tool, default bash>-<cmd>`, the SAME key the hook path
/// derives from that prompt's `tool_input.command`. Any other reason text →
/// `permission-prompt-<≤6 words>-<hash6(normalized head)>`,
/// where the head is the normalized reason line up to its first `": "` (the
/// part after it names the instance, not the class). Otherwise
/// `permission-prompt-<tool>-<first command token>`, then
/// `permission-prompt-<tool>` (tool parsed from the stock message when not
/// given), then `permission-prompt-unknown`.
pub fn classify(text: Option<&str>, tool_name: Option<&str>, command: Option<&str>) -> PromptClass {
    let text = crate::str_utils::truncate_str(text.unwrap_or(""), MAX_CLASSIFY_INPUT_BYTES);
    let command = command.map(|c| crate::str_utils::truncate_str(c, MAX_CLASSIFY_INPUT_BYTES));
    let tool = tool_name
        .map(sanitize_token)
        .filter(|t| !t.is_empty())
        .or_else(|| {
            STOCK_TOOL_RE
                .captures(text)
                .map(|c| sanitize_token(&c[1]))
                .filter(|t| !t.is_empty())
        });
    let reason = extract_reason(text, command);

    // A built-in command-safety reason names its command ("Dangerous rm
    // operation on …"). Neither hook payload carries reason text (Phase 0,
    // CLI 2.1.303), so the hook path keys such a prompt on the command's first
    // token; a PASTED reason must land on that same key, or a manual `record`
    // opens a second dossier for a class the hook already counts.
    let dangerous_cmd = reason.as_deref().and_then(|r| {
        DANGEROUS_OP_RE
            .captures(r.lines().next().unwrap_or(r))
            .map(|c| sanitize_token(&c[1]))
            .filter(|t| !t.is_empty())
    });

    let class_key = match (&reason, dangerous_cmd) {
        (_, Some(cmd)) => {
            let tool = tool.as_deref().unwrap_or(DEFAULT_SAFETY_TOOL);
            format!("{CLASS_KEY_PREFIX}-{tool}-{cmd}")
        }
        (Some(reason), None) => {
            // Words and hash come from REDACTED text: a secret in a pasted
            // reason must never reach the class key, which leaves the machine.
            let first_line = redact_and_strip(reason.lines().next().unwrap_or(reason));
            let normalized = normalize_reason(&first_line);
            let head = match normalized.split_once(": ") {
                Some((head, _)) if head.split_whitespace().count() >= 2 => head.to_string(),
                _ => normalized.clone(),
            };
            let words: Vec<String> = PLACEHOLDER_RE
                .replace_all(&head, " ")
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|w| !w.is_empty())
                .take(MAX_KEY_WORDS)
                .map(str::to_string)
                .collect();
            let words = if words.is_empty() {
                "reason".to_string()
            } else {
                words.join("-")
            };
            format!("{CLASS_KEY_PREFIX}-{words}-{}", hash_hex(&head, 6))
        }
        (None, None) => {
            // Keyed on the REDACTED command: a credential in the command word
            // (a URL with userinfo, a bare JWT) must never reach the key.
            let cmd_tok = command
                .map(redact_and_strip)
                .and_then(|c| first_command_token(&c));
            match (&tool, cmd_tok) {
                (Some(t), Some(c)) => format!("{CLASS_KEY_PREFIX}-{t}-{c}"),
                (Some(t), None) => format!("{CLASS_KEY_PREFIX}-{t}"),
                (None, Some(c)) => format!("{CLASS_KEY_PREFIX}-unknown-{c}"),
                (None, None) => format!("{CLASS_KEY_PREFIX}-unknown"),
            }
        }
    };
    let dossier_slug = format!("{DOSSIER_SLUG_PREFIX}{}", hash_hex(&class_key, 8));
    PromptClass {
        class_key,
        dossier_slug,
        reason,
        tool,
    }
}

// =============================================================================
// D6 — redaction
// =============================================================================

/// One redacted example a bucket keeps.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RedactedExample {
    /// The command, redacted, assignment values stripped, ≤ 300 chars.
    pub command: Option<String>,
    /// The last two path components of `cwd`.
    pub cwd: Option<String>,
}

/// A shell assignment at a token start (`FOO=bar`, `; X=1`). The value is
/// replaced; `--flag=value` is left alone (a `-` precedes it).
static ASSIGNMENT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(^|[\s;&|(`])([A-Za-z_][A-Za-z0-9_]*)=("[^"]*"|'[^']*'|[^\s;&|)`]*)"#)
        .expect("assignment regex is a valid literal")
});

/// The mask an assignment value becomes.
pub const ASSIGNMENT_MASK: &str = "***";

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max).and_then(|(idx, _)| s.get(..idx)) {
        Some(head) => format!("{head}…"),
        None => s.to_string(),
    }
}

/// Redact free text through the one redactor.
fn redact_text(s: &str) -> String {
    String::from_utf8_lossy(&crate::session::redact::redact_secrets(s.as_bytes())).into_owned()
}

/// D6 for a command: the one redactor, then assignment values stripped, then
/// truncated.
pub fn redact_command(command: &str) -> String {
    truncate_chars(&redact_and_strip(command), COMMAND_EXAMPLE_MAX_CHARS)
}

/// The one redactor, then `VAR=value` assignment values stripped — the D6
/// sweep without a length cap.
fn redact_and_strip(text: &str) -> String {
    let swept = redact_text(text);
    ASSIGNMENT_RE
        .replace_all(&swept, |c: &regex::Captures| {
            format!("{}{}={ASSIGNMENT_MASK}", &c[1], &c[2])
        })
        .into_owned()
}

/// The cap on a pasted prompt's redacted text (`POST /permission-prompts/classify`).
pub const PASTED_TEXT_MAX_CHARS: usize = 4000;

/// D6 for a whole pasted prompt: the same sweep a captured example gets, capped
/// at [`PASTED_TEXT_MAX_CHARS`], so the `/permission-prompts record` command
/// posts runner-redacted text rather than redacting it a second way.
pub fn redact_pasted(text: &str) -> String {
    truncate_chars(&redact_and_strip(text), PASTED_TEXT_MAX_CHARS)
}

/// D6 for a cwd: its last two path components only (`/home/u/ws/repo` →
/// `ws/repo`), separators of either OS.
pub fn reduce_cwd(cwd: &str) -> String {
    let parts: Vec<&str> = cwd.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    let tail = &parts[parts.len().saturating_sub(2)..];
    tail.join("/")
}

/// D6: build the example a bucket stores.
pub fn redact_example(command: Option<&str>, cwd: Option<&str>) -> RedactedExample {
    RedactedExample {
        command: command
            .map(|c| crate::str_utils::truncate_str(c, MAX_CLASSIFY_INPUT_BYTES).trim())
            .filter(|c| !c.is_empty())
            .map(redact_command),
        cwd: cwd
            .map(|c| truncate_chars(&reduce_cwd(c), CWD_EXAMPLE_MAX_CHARS))
            .filter(|c| !c.is_empty()),
    }
}

// =============================================================================
// D4 — the aggregator
// =============================================================================

/// Which hook payload an [`Observation`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    /// `Notification` with `notification_type: permission_prompt` — only a
    /// stock `message` (Phase 0: seen ~6 s after the dialog's
    /// `PermissionRequest`; whether it repeats is unmeasured).
    Notification,
    /// `PermissionRequest` — fires before the dialog, with `tool_name`,
    /// `tool_input` and `permission_suggestions`.
    PermissionRequest,
}

/// One prompt, observed and classified, ready to be counted.
#[derive(Debug, Clone)]
pub struct Observation {
    pub class: PromptClass,
    pub kind: PayloadKind,
    /// `Some(true)` when `permission_suggestions` was present and EMPTY — the
    /// shape of every built-in safety/parser ask (Phase 0, CLI 2.1.303);
    /// `Some(false)` when non-empty (an ordinary rule prompt); `None` when the
    /// payload had no such key (every `Notification`).
    pub safety_class: Option<bool>,
    pub permission_mode: Option<String>,
    /// Already redacted (D6) — built outside the aggregator lock.
    pub example: RedactedExample,
    /// The class's reason text, already redacted and capped.
    pub reason: Option<String>,
}

impl Observation {
    /// Build an observation, doing ALL redaction here — before the caller
    /// takes the aggregator lock.
    pub fn new(
        class: PromptClass,
        kind: PayloadKind,
        safety_class: Option<bool>,
        permission_mode: Option<&str>,
        command: Option<&str>,
        cwd: Option<&str>,
    ) -> Self {
        let reason = class
            .reason
            .as_deref()
            .map(|r| truncate_chars(&redact_and_strip(r), REASON_MAX_CHARS));
        Self {
            class,
            kind,
            safety_class,
            permission_mode: permission_mode
                .filter(|m| !m.is_empty())
                .map(|m| m.chars().take(MAX_PERMISSION_MODE_CHARS).collect()),
            example: redact_example(command, cwd),
            reason,
        }
    }
}

/// The counts for one (class, UTC hour).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassBucket {
    pub dossier_slug: String,
    pub class_key: String,
    /// Redacted, ≤ [`REASON_MAX_CHARS`].
    pub reason: Option<String>,
    pub tool: Option<String>,
    /// Start of the UTC hour this bucket counts.
    pub hour_start: DateTime<Utc>,
    pub count: u64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub permission_modes: BTreeSet<String>,
    /// Observations whose payload marked a built-in safety ask
    /// (`permission_suggestions: []`).
    pub safety_asks: u64,
    /// Observations whose payload marked an ordinary rule prompt
    /// (non-empty `permission_suggestions`).
    pub ordinary_asks: u64,
    /// Up to [`MAX_EXAMPLES_PER_BUCKET`] distinct redacted examples.
    pub examples: Vec<RedactedExample>,
}

impl ClassBucket {
    /// `Some(true)` when every observation that said anything was a safety
    /// ask, `Some(false)` when every one was an ordinary prompt, `None` when
    /// none said (Notification-only) or they disagreed.
    pub fn safety_class(&self) -> Option<bool> {
        match (self.safety_asks, self.ordinary_asks) {
            (0, 0) => None,
            (_, 0) => Some(true),
            (0, _) => Some(false),
            _ => None,
        }
    }

    /// JSON view — the shape Phase 3's flush builds its finding body from.
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "dossier_slug": self.dossier_slug,
            "class_key": self.class_key,
            "reason": self.reason,
            "tool": self.tool,
            "hour_start": self.hour_start.to_rfc3339(),
            "count": self.count,
            "first_seen": self.first_seen.to_rfc3339(),
            "last_seen": self.last_seen.to_rfc3339(),
            "permission_modes": self.permission_modes,
            "safety_class": self.safety_class(),
            "safety_asks": self.safety_asks,
            "ordinary_asks": self.ordinary_asks,
            "examples": self.examples,
        })
    }

    /// The finding body Phase 3's hourly flush posts: `count: <n>` as the
    /// FIRST line (the line the `/permission-prompts` report reads), then the
    /// class, the window, the reason text and the redacted examples.
    pub fn render_finding_body(&self) -> String {
        let modes: Vec<&str> = self.permission_modes.iter().map(String::as_str).collect();
        let safety = match self.safety_class() {
            Some(true) => "true",
            Some(false) => "false",
            None => "unknown",
        };
        let mut lines = vec![
            format!("count: {}", self.count),
            format!("class: {}", self.class_key),
            format!("dossier_slug: {}", self.dossier_slug),
        ];
        if let Some(tool) = &self.tool {
            lines.push(format!("tool: {tool}"));
        }
        lines.push(format!(
            "window: {} .. {} (first {}, last {})",
            self.hour_start.to_rfc3339(),
            (self.hour_start + chrono::Duration::hours(1)).to_rfc3339(),
            self.first_seen.to_rfc3339(),
            self.last_seen.to_rfc3339(),
        ));
        lines.push(format!(
            "permission_modes: {}",
            if modes.is_empty() {
                "unknown".to_string()
            } else {
                modes.join(", ")
            }
        ));
        lines.push(format!("safety_class: {safety}"));
        if let Some(reason) = &self.reason {
            lines.push(String::new());
            lines.push("reason:".to_string());
            lines.push(reason.clone());
        }
        if !self.examples.is_empty() {
            lines.push(String::new());
            lines.push("examples (redacted):".to_string());
            for ex in &self.examples {
                lines.push(format!(
                    "- cwd={} command={}",
                    ex.cwd.as_deref().unwrap_or("-"),
                    ex.command.as_deref().unwrap_or("-"),
                ));
            }
        }
        let mut out = lines.join("\n");
        out.push('\n');
        out
    }
}

fn hour_bucket(t: DateTime<Utc>) -> i64 {
    t.timestamp().div_euclid(3600)
}

/// In-memory per-(class, hour) counter. The process-global instance is behind
/// [`global`]; tests build their own.
#[derive(Debug, Default)]
pub struct Aggregator {
    buckets: BTreeMap<(String, i64), ClassBucket>,
    dropped: u64,
    /// Per terminal id, the last prompt signal: a counted `PermissionRequest`
    /// or a counted or suppressed `Notification` (see [`DEDUP_WINDOW_SECS`]).
    last_signal: HashMap<String, DateTime<Utc>>,
}

/// What [`Aggregator::observe`] did with one observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObserveOutcome {
    /// Counted in its (class, hour) bucket.
    Counted(PromptClass),
    /// A `Notification` within [`DEDUP_WINDOW_SECS`] of a counted
    /// `PermissionRequest` on the same terminal — the same dialog, not counted.
    Deduplicated(PromptClass),
    /// A permission prompt the aggregator could not hold: it was at
    /// [`MAX_BUCKETS`] and every held bucket was newer than this one.
    Dropped(PromptClass),
    /// Not a permission prompt (or unparseable), or refused at the door:
    /// nothing recorded.
    Ignored,
}

impl ObserveOutcome {
    pub fn counted(&self) -> bool {
        matches!(self, Self::Counted(_))
    }

    pub fn class(&self) -> Option<&PromptClass> {
        match self {
            Self::Counted(c) | Self::Deduplicated(c) | Self::Dropped(c) => Some(c),
            Self::Ignored => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Counted(_) => "counted",
            Self::Deduplicated(_) => {
                "deduplicated: a PermissionRequest for this dialog was already counted"
            }
            Self::Dropped(_) => "dropped: the aggregator is full of newer buckets",
            Self::Ignored => "not a permission prompt",
        }
    }
}

impl Aggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Count `obs` from terminal `terminal_id`. A `PermissionRequest` always
    /// counts (it fires once, before its dialog). A `Notification` is NOT
    /// counted when it arrives within [`DEDUP_WINDOW_SECS`] of the last prompt
    /// signal on that terminal — the echo of a counted `PermissionRequest`, or
    /// a repeat of an earlier `Notification` for a dialog still open — and it
    /// slides the window. A `Notification` with no recent signal (an older CLI,
    /// or a carrier that registers only `Notification`) counts, under its
    /// coarser class.
    pub fn observe(
        &mut self,
        terminal_id: &str,
        obs: Observation,
        now: DateTime<Utc>,
    ) -> ObserveOutcome {
        let window = chrono::Duration::seconds(DEDUP_WINDOW_SECS);
        let recent = self.last_signal.get(terminal_id).is_some_and(|at| {
            let age = now.signed_duration_since(*at);
            age >= chrono::Duration::zero() && age < window
        });
        // Every signal (counted or suppressed) refreshes its terminal's entry,
        // so a dialog left open stays ONE prompt; stale entries are pruned.
        self.last_signal
            .retain(|_, at| now.signed_duration_since(*at) < window);
        if !terminal_id.is_empty() {
            self.last_signal.insert(terminal_id.to_string(), now);
        }
        if obs.kind == PayloadKind::Notification && recent {
            return ObserveOutcome::Deduplicated(obs.class);
        }
        let class = obs.class.clone();
        if self.record(obs, now) {
            ObserveOutcome::Counted(class)
        } else {
            ObserveOutcome::Dropped(class)
        }
    }

    /// Count one observation at `now`. At [`MAX_BUCKETS`] a NEW bucket evicts
    /// the oldest-hour bucket (its count goes to [`Self::dropped`]); an
    /// observation older than every held bucket is itself dropped and this
    /// returns `false`.
    pub fn record(&mut self, obs: Observation, now: DateTime<Utc>) -> bool {
        let hour = hour_bucket(now);
        let key = (obs.class.dossier_slug.clone(), hour);
        if !self.buckets.contains_key(&key) && self.buckets.len() >= MAX_BUCKETS {
            let oldest = self
                .buckets
                .keys()
                .min_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)))
                .cloned();
            match oldest {
                Some(old) if old.1 <= hour => {
                    if let Some(evicted) = self.buckets.remove(&old) {
                        self.dropped += evicted.count;
                    }
                }
                _ => {
                    self.dropped += 1;
                    return false;
                }
            }
        }
        let example = obs.example;
        let bucket = self.buckets.entry(key).or_insert_with(|| ClassBucket {
            dossier_slug: obs.class.dossier_slug.clone(),
            class_key: obs.class.class_key.clone(),
            reason: obs.reason.clone(),
            tool: obs.class.tool.clone(),
            hour_start: Utc.timestamp_opt(hour * 3600, 0).single().unwrap_or(now),
            count: 0,
            first_seen: now,
            last_seen: now,
            permission_modes: BTreeSet::new(),
            safety_asks: 0,
            ordinary_asks: 0,
            examples: Vec::new(),
        });
        bucket.count += 1;
        bucket.first_seen = bucket.first_seen.min(now);
        bucket.last_seen = bucket.last_seen.max(now);
        match obs.safety_class {
            Some(true) => bucket.safety_asks += 1,
            Some(false) => bucket.ordinary_asks += 1,
            None => {}
        }
        if let Some(mode) = obs.permission_mode {
            if bucket.permission_modes.len() < MAX_PERMISSION_MODES
                || bucket.permission_modes.contains(&mode)
            {
                bucket.permission_modes.insert(mode);
            }
        }
        let has_content = example.command.is_some() || example.cwd.is_some();
        if has_content
            && bucket.examples.len() < MAX_EXAMPLES_PER_BUCKET
            && !bucket.examples.contains(&example)
        {
            bucket.examples.push(example);
        }
        true
    }

    /// Every live bucket, oldest hour first.
    pub fn snapshot(&self) -> Vec<ClassBucket> {
        let mut out: Vec<ClassBucket> = self.buckets.values().cloned().collect();
        out.sort_by(|a, b| (a.hour_start, &a.dossier_slug).cmp(&(b.hour_start, &b.dossier_slug)));
        out
    }

    /// Remove and return every bucket whose hour has ENDED at `now` — the
    /// current hour keeps counting. Phase 3's flush calls this.
    pub fn drain_completed_hours(&mut self, now: DateTime<Utc>) -> Vec<ClassBucket> {
        let current = hour_bucket(now);
        let done: Vec<(String, i64)> = self
            .buckets
            .keys()
            .filter(|(_, h)| *h < current)
            .cloned()
            .collect();
        let mut out: Vec<ClassBucket> = done
            .into_iter()
            .filter_map(|k| self.buckets.remove(&k))
            .collect();
        out.sort_by(|a, b| (a.hour_start, &a.dossier_slug).cmp(&(b.hour_start, &b.dossier_slug)));
        out
    }

    /// Observations lost to the [`MAX_BUCKETS`] bound: the counts of evicted
    /// buckets plus refused observations.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// The process-global aggregator the HTTP intake feeds.
pub fn global() -> &'static Mutex<Aggregator> {
    static AGG: OnceLock<Mutex<Aggregator>> = OnceLock::new();
    AGG.get_or_init(|| Mutex::new(Aggregator::new()))
}

fn with_global<R>(f: impl FnOnce(&mut Aggregator) -> R) -> R {
    // A poisoned lock still holds valid counts; never let it break intake.
    let mut guard = global().lock().unwrap_or_else(|p| p.into_inner());
    f(&mut guard)
}

/// Snapshot of the process-global aggregator.
pub fn snapshot() -> Vec<ClassBucket> {
    with_global(|a| a.snapshot())
}

/// The process-global aggregator's [`Aggregator::dropped`] count.
pub fn dropped() -> u64 {
    with_global(|a| a.dropped())
}

/// Drain the process-global aggregator's completed hours.
pub fn drain_completed_hours(now: DateTime<Utc>) -> Vec<ClassBucket> {
    with_global(|a| a.drain_completed_hours(now))
}

// =============================================================================
// The intake
// =============================================================================

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// The stock `Notification` message for a permission dialog.
const STOCK_PERMISSION_MESSAGE: &str = "claude needs your permission";

/// Parse one hook payload into an [`Observation`], or `None` when it is not a
/// permission prompt. Recognized shapes:
///
/// - `PermissionRequest` (`hook_event_name == "PermissionRequest"`, or a
///   `tool_name` + `tool_input` pair with no `notification_type`): the class
///   comes from `tool_name` + `tool_input.command`, and
///   `permission_suggestions` marks a safety ask (D3 fallback key — Phase 0
///   found reason text in neither payload);
/// - `Notification` with `notification_type == "permission_prompt"`, or with
///   no `notification_type` and the stock "Claude needs your permission…"
///   `message`: the class comes from the tool named in the message.
pub fn parse_payload(payload: &Value) -> Option<Observation> {
    let notification_type = str_field(payload, "notification_type");
    let message = str_field(payload, "message");
    let is_permission_request = str_field(payload, "hook_event_name") == Some("PermissionRequest")
        || (notification_type.is_none()
            && payload.get("tool_name").is_some()
            && payload.get("tool_input").is_some());
    let is_notification_prompt = notification_type == Some("permission_prompt")
        || (notification_type.is_none()
            && message.is_some_and(|m| {
                m.trim_start()
                    .to_lowercase()
                    .starts_with(STOCK_PERMISSION_MESSAGE)
            }));
    let kind = if is_permission_request {
        PayloadKind::PermissionRequest
    } else if is_notification_prompt {
        PayloadKind::Notification
    } else {
        return None;
    };
    let tool_input = payload.get("tool_input");
    let command = tool_input
        .and_then(|t| str_field(t, "command"))
        .map(str::to_string);
    // `reason` is on neither payload as of CLI 2.1.303 (Phase 0); it is read in
    // case a later CLI adds the built-in check's reason text there.
    let text = message.or_else(|| str_field(payload, "reason"));
    let class = classify(text, str_field(payload, "tool_name"), command.as_deref());
    let safety_class = payload
        .get("permission_suggestions")
        .and_then(Value::as_array)
        .map(Vec::is_empty);
    Some(Observation::new(
        class,
        kind,
        safety_class,
        str_field(payload, "permission_mode"),
        command.as_deref(),
        str_field(payload, "cwd"),
    ))
}

/// The `POST /sessions/{id}/notification` intake: called with the raw body.
/// Lenient (garbage, empty, non-JSON → [`ObserveOutcome::Ignored`]),
/// fail-open, and never panics.
pub fn observe_notification(terminal_id: &str, body: &[u8]) -> ObserveOutcome {
    observe_at(terminal_id, body, Utc::now())
}

/// The `POST /sessions/{id}/permission-request` intake — same posture as
/// [`observe_notification`]; the payload's own shape decides its kind.
pub fn observe_permission_request(terminal_id: &str, body: &[u8]) -> ObserveOutcome {
    observe_at(terminal_id, body, Utc::now())
}

fn observe_at(terminal_id: &str, body: &[u8], now: DateTime<Utc>) -> ObserveOutcome {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Ok(payload) = serde_json::from_slice::<Value>(body) else {
            return ObserveOutcome::Ignored;
        };
        let Some(obs) = parse_payload(&payload) else {
            return ObserveOutcome::Ignored;
        };
        with_global(|a| a.observe(terminal_id, obs, now))
    }));
    match result {
        Ok(outcome) => {
            if let Some(class) = outcome.class() {
                tracing::debug!(
                    terminal = %terminal_id,
                    class_key = %class.class_key,
                    dossier_slug = %class.dossier_slug,
                    outcome = outcome.label(),
                    "permission_prompt_capture: observed"
                );
            }
            outcome
        }
        Err(_) => {
            tracing::warn!(
                terminal = %terminal_id,
                "permission_prompt_capture: classifier panicked; nothing recorded"
            );
            ObserveOutcome::Ignored
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The prompt as pasted from the blocked pane on 2026-10-03 (plan
    /// `2026-10-03-runner-sessions-stop-on-builtin-command-safety-prompts`).
    const PROMPT_2026_10_03: &str = "… if git show $ref:$p > $f 2>/dev/null; then :; else echo \"MISSING $ref $p\"; rm -f $f; fi …

Dangerous rm operation on possibly-empty variable path: $f in `rm -f $f`
(use a literal path: $f is set in this command from $S/$d/$(echo $p | tr / _),
which is the filesystem root when the variable in it is empty)

Do you want to proceed?
❯ 1. Yes
  2. No";

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn the_2026_10_03_prompt_classifies_on_its_command() {
        let c = classify(Some(PROMPT_2026_10_03), Some("Bash"), None);
        assert_eq!(c.class_key, "permission-prompt-bash-rm", "{c:?}");
        assert!(c
            .reason
            .as_deref()
            .unwrap()
            .starts_with("Dangerous rm operation"));
        assert!(c.reason.as_deref().unwrap().contains("filesystem root"));
        assert_eq!(c.tool.as_deref(), Some("bash"));
        assert!(c.dossier_slug.starts_with("ppc-"));
        assert_eq!(c.dossier_slug.len(), "ppc-".len() + 8);
    }

    #[test]
    fn a_different_variable_or_path_is_the_same_class() {
        let base = classify(Some(PROMPT_2026_10_03), None, None);
        for variant in [
            "Dangerous rm operation on possibly-empty variable path: $target in `rm -rf $target`",
            "Dangerous rm operation on possibly-empty variable path: ${OUT_DIR} in `rm -f ${OUT_DIR}/x`",
            "Dangerous rm operation on possibly-empty variable path: $dir/sub in `rm -r $dir/sub` (line 12)",
            "  Dangerous rm operation on possibly-empty variable path: $X in `rm $X`\n\nDo you want to proceed?",
        ] {
            let c = classify(Some(variant), None, None);
            assert_eq!(c.class_key, base.class_key, "variant {variant:?}");
            assert_eq!(c.dossier_slug, base.dossier_slug);
        }
    }

    /// Phase 0 (CLI 2.1.303): the hook sees only `tool_name` + `tool_input`, so
    /// a PASTED "Dangerous <cmd> operation" reason must key exactly as the hook
    /// path keys the same prompt — or a manual `record` opens a second dossier.
    #[test]
    fn a_pasted_dangerous_reason_keys_like_the_hook_path() {
        for (pasted, command) in [
            (
                PROMPT_2026_10_03,
                "if git show $ref:$p > $f 2>/dev/null; then :; else echo \"MISSING $ref $p\"; rm -f $f; fi",
            ),
            (
                PROMPT_2026_10_03,
                "if git show \"$ref:$p\" > \"$f\"; then rm -rf \"$X/out\"; fi",
            ),
            (
                "Dangerous rm operation on possibly-empty variable path: $f in `rm -f $f`",
                "rm -f $f",
            ),
            (
                "Dangerous rmdir operation on possibly-empty variable path: $PROBE_EMPTY_VAR/",
                "rmdir $PROBE_EMPTY_VAR/",
            ),
        ] {
            let manual = classify(Some(pasted), None, None);
            let hook = parse_payload(&serde_json::json!({
                "hook_event_name": "PermissionRequest",
                "tool_name": "Bash",
                "tool_input": {"command": command, "description": "x"},
                "permission_suggestions": [],
            }))
            .unwrap()
            .class;
            assert_eq!(manual.class_key, hook.class_key, "{command:?}");
            assert_eq!(manual.dossier_slug, hook.dossier_slug);
        }
        assert_eq!(
            classify(
                Some("Dangerous rmdir operation on possibly-empty variable path: $X/"),
                None,
                None
            )
            .class_key,
            "permission-prompt-bash-rmdir"
        );
    }

    /// The command-token rule (module doc): a removal command anywhere in
    /// command position wins; otherwise the first non-keyword command word.
    #[test]
    fn the_command_token_rule_handles_compound_commands() {
        let key = |cmd: &str| classify(None, Some("Bash"), Some(cmd)).class_key;
        for (cmd, want) in [
            (
                "if git show a:b > c; then rm -rf \"$X/y\"; fi",
                "permission-prompt-bash-rm",
            ),
            ("cd /x && npm test", "permission-prompt-bash-cd"),
            ("npm test || rmdir $D/", "permission-prompt-bash-rmdir"),
            ("for f in *; do rm $f; done", "permission-prompt-bash-rm"),
            ("(cd x; sudo /bin/rm -f y)", "permission-prompt-bash-rm"),
            ("X=1 env Y=2 nohup git push", "permission-prompt-bash-git"),
            (
                "while true; do sleep 1; done",
                "permission-prompt-bash-true",
            ),
            ("echo \"a; rm b\" | grep rmx", "permission-prompt-bash-echo"),
            (
                "ls
rm -f z",
                "permission-prompt-bash-rm",
            ),
            ("! grep -q x f", "permission-prompt-bash-grep"),
        ] {
            assert_eq!(key(cmd), want, "{cmd:?}");
        }
        assert_eq!(
            classify(None, Some("Bash"), Some("if then fi")).class_key,
            "permission-prompt-bash"
        );
    }

    #[test]
    fn a_different_reason_is_a_different_class() {
        let a = classify(Some(PROMPT_2026_10_03), None, None);
        let b = classify(
            Some("Command contains output redirection to a device path: `> /dev/sda`"),
            None,
            None,
        );
        assert_ne!(a.class_key, b.class_key);
        assert_ne!(a.dossier_slug, b.dossier_slug);
    }

    #[test]
    fn the_stock_message_falls_back_to_tool_and_command() {
        let stock = "Claude needs your permission to use Bash";
        let c = classify(Some(stock), None, Some("FOO=1 /usr/bin/rm -f x"));
        assert_eq!(c.class_key, "permission-prompt-bash-rm");
        assert!(c.reason.is_none());
        let c = classify(Some(stock), None, None);
        assert_eq!(c.class_key, "permission-prompt-bash");
        let c = classify(
            Some("Claude needs your permission to use mcp__coord-mcp__coord_x"),
            None,
            None,
        );
        assert_eq!(c.class_key, "permission-prompt-mcp-coord-mcp-coord-x");
        let c = classify(None, None, None);
        assert_eq!(c.class_key, "permission-prompt-unknown");
        let c = classify(None, Some("Bash"), Some("git push origin main"));
        assert_eq!(c.class_key, "permission-prompt-bash-git");
    }

    #[test]
    fn a_pasted_command_line_is_not_taken_for_the_reason() {
        let pasted = "rm -f $f\n\nClaude needs your permission to use Bash\n❯ 1. Yes";
        let c = classify(Some(pasted), Some("Bash"), Some("rm -f $f"));
        assert!(c.reason.is_none(), "{c:?}");
        assert_eq!(c.class_key, "permission-prompt-bash-rm");
    }

    #[test]
    fn classification_is_stable_across_calls() {
        // Pinned digest: a change here re-keys every dossier, so it must be a
        // deliberate one.
        let c = classify(None, Some("Bash"), Some("rm -f x"));
        assert_eq!(c.class_key, "permission-prompt-bash-rm");
        assert_eq!(
            c.dossier_slug,
            format!("ppc-{}", hash_hex("permission-prompt-bash-rm", 8))
        );
        assert_eq!(classify(None, Some("Bash"), Some("rm -f x")), c);
    }

    /// D3: the findings steward folds a slug sharing ≥ 60 % of its hyphen
    /// tokens with an existing head. Dossier slugs across distinct classes
    /// must never come near that.
    #[test]
    fn dossier_slugs_share_under_60_percent_of_hyphen_tokens() {
        let classes = [
            classify(
                Some("Dangerous rmdir operation on possibly-empty variable path: $X/"),
                None,
                None,
            ),
            classify(
                Some("Command contains output redirection to a device path: x"),
                None,
                None,
            ),
            classify(None, Some("Bash"), Some("rm -f x")),
            classify(None, Some("Bash"), Some("git push")),
            classify(None, Some("Bash"), Some("curl http://x")),
            classify(Some("Claude needs your permission to use Edit"), None, None),
            classify(
                Some("Claude needs your permission to use Write"),
                None,
                None,
            ),
            classify(None, None, None),
        ];
        // The readable keys DO overlap heavily — which is why they are not the slug.
        let toks = |s: &str| s.split('-').map(str::to_string).collect::<BTreeSet<_>>();
        for (i, a) in classes.iter().enumerate() {
            for b in &classes[i + 1..] {
                assert_ne!(a.dossier_slug, b.dossier_slug, "{a:?} vs {b:?}");
                let (ta, tb) = (toks(&a.dossier_slug), toks(&b.dossier_slug));
                let shared = ta.intersection(&tb).count();
                let ratio = shared as f64 / ta.len().min(tb.len()) as f64;
                assert!(
                    ratio < 0.6,
                    "{} vs {} share {ratio}",
                    a.dossier_slug,
                    b.dossier_slug
                );
            }
        }
    }

    // ---- D6 redaction ------------------------------------------------------

    #[test]
    fn redaction_masks_a_jwt_an_assignment_and_url_userinfo() {
        // Assembled at runtime so source secret scanners see no literal.
        let jwt = format!(
            "eyJ{}.eyJ{}.{}",
            "hbGciOiJIUzI1NiJ9", "zdWIiOiJ4In0", "c2lnbmF0dXJl"
        );
        let cmd = format!(
            "API_KEY={} DEPLOY_ENV=prod curl -H 'X-T: {jwt}' https://{}:{}@git.example.com/r.git --flag=keep",
            "sk-live-1", "alice", "hunter2"
        );
        let out = redact_command(&cmd);
        assert!(!out.contains("sk-live-1"), "{out}");
        assert!(!out.contains("prod"), "{out}");
        assert!(!out.contains(&jwt), "{out}");
        assert!(!out.contains("hunter2") && !out.contains("alice"), "{out}");
        assert!(out.contains("API_KEY=***"), "{out}");
        assert!(out.contains("DEPLOY_ENV=***"), "{out}");
        assert!(out.contains("https://***@git.example.com/r.git"), "{out}");
        assert!(out.contains("--flag=keep"), "{out}");
    }

    #[test]
    fn redaction_truncates_the_command_and_reduces_cwd() {
        let long = "x".repeat(1000);
        let out = redact_command(&long);
        assert_eq!(out.chars().count(), COMMAND_EXAMPLE_MAX_CHARS + 1);
        assert!(out.ends_with('…'));
        assert_eq!(reduce_cwd("/home/u/ws/repo"), "ws/repo");
        assert_eq!(
            reduce_cwd(r"D:\qontinui-root\agent-worktrees\abc\qontinui-runner"),
            "abc/qontinui-runner"
        );
        assert_eq!(reduce_cwd("/repo"), "repo");
        assert_eq!(reduce_cwd(""), "");
    }

    // ---- D4 aggregator -----------------------------------------------------

    fn obs(class: &PromptClass, mode: &str, cmd: &str) -> Observation {
        Observation::new(
            class.clone(),
            PayloadKind::PermissionRequest,
            None,
            Some(mode),
            Some(cmd),
            Some("/home/u/ws/repo"),
        )
    }

    #[test]
    fn aggregator_buckets_by_class_and_hour() {
        let mut agg = Aggregator::new();
        let a = classify(None, Some("Bash"), Some("rm x"));
        let b = classify(None, Some("Bash"), Some("git x"));
        agg.record(
            obs(&a, "bypassPermissions", "rm a"),
            utc("2026-10-14T10:05:00Z"),
        );
        agg.record(obs(&a, "default", "rm b"), utc("2026-10-14T10:55:00Z"));
        agg.record(obs(&a, "default", "rm b"), utc("2026-10-14T10:56:00Z"));
        agg.record(obs(&a, "default", "rm c"), utc("2026-10-14T10:57:00Z"));
        agg.record(obs(&a, "default", "rm d"), utc("2026-10-14T10:58:00Z"));
        agg.record(obs(&b, "default", "git x"), utc("2026-10-14T10:30:00Z"));
        agg.record(obs(&a, "default", "rm e"), utc("2026-10-14T11:00:00Z"));

        let snap = agg.snapshot();
        assert_eq!(snap.len(), 3);
        let a10 = snap
            .iter()
            .find(|x| {
                x.dossier_slug == a.dossier_slug && x.hour_start == utc("2026-10-14T10:00:00Z")
            })
            .unwrap();
        assert_eq!(a10.count, 5);
        assert_eq!(a10.first_seen, utc("2026-10-14T10:05:00Z"));
        assert_eq!(a10.last_seen, utc("2026-10-14T10:58:00Z"));
        assert_eq!(
            a10.permission_modes.iter().cloned().collect::<Vec<_>>(),
            vec!["bypassPermissions".to_string(), "default".to_string()]
        );
        assert_eq!(
            a10.examples.len(),
            MAX_EXAMPLES_PER_BUCKET,
            "deduped, capped"
        );
        assert_eq!(a10.examples[1].command.as_deref(), Some("rm b"));
        assert_eq!(a10.examples[0].cwd.as_deref(), Some("ws/repo"));
        assert_eq!(a10.to_json()["count"], 5);

        // Only completed hours drain; the current one keeps counting.
        let drained = agg.drain_completed_hours(utc("2026-10-14T11:10:00Z"));
        assert_eq!(drained.len(), 2);
        assert!(drained
            .iter()
            .all(|x| x.hour_start == utc("2026-10-14T10:00:00Z")));
        let left = agg.snapshot();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].hour_start, utc("2026-10-14T11:00:00Z"));
        assert!(agg
            .drain_completed_hours(utc("2026-10-14T11:10:00Z"))
            .is_empty());
    }

    #[test]
    fn at_the_bound_a_new_bucket_evicts_the_oldest_hour() {
        let mut agg = Aggregator::new();
        let ten = utc("2026-10-14T10:00:00Z");
        let c0 = classify(None, Some("Bash"), Some("first0"));
        agg.record(obs(&c0, "default", "x"), utc("2026-10-14T09:30:00Z"));
        agg.record(obs(&c0, "default", "x"), utc("2026-10-14T09:31:00Z"));
        for i in 1..MAX_BUCKETS {
            let c = classify(None, Some("Bash"), Some(&format!("cmd{i}")));
            assert!(agg.record(obs(&c, "default", "x"), ten));
        }
        assert_eq!(agg.snapshot().len(), MAX_BUCKETS);
        let fresh = classify(None, Some("Bash"), Some("fresh"));
        assert!(agg.record(obs(&fresh, "default", "x"), ten));
        assert_eq!(agg.snapshot().len(), MAX_BUCKETS);
        assert_eq!(agg.dropped(), 2, "the evicted 09:00 bucket held 2");
        assert!(!agg
            .snapshot()
            .iter()
            .any(|b| b.dossier_slug == c0.dossier_slug));
        assert!(agg
            .snapshot()
            .iter()
            .any(|b| b.dossier_slug == fresh.dossier_slug));
        // An observation older than every held bucket is itself dropped.
        let stale = classify(None, Some("Bash"), Some("stale"));
        assert!(!agg.record(obs(&stale, "default", "x"), utc("2026-10-14T08:00:00Z")));
        assert_eq!(agg.dropped(), 3);
        let mut o = obs(&stale, "default", "x");
        o.kind = PayloadKind::PermissionRequest;
        assert!(matches!(
            agg.observe("t", o, utc("2026-10-14T08:00:00Z")),
            ObserveOutcome::Dropped(_)
        ));
    }

    #[test]
    fn a_secret_in_a_pasted_reason_never_reaches_the_class_key() {
        let c = classify(
            Some(&format!(
                "Command sends TOKEN={} to a remote host",
                "zzsecret77"
            )),
            None,
            None,
        );
        assert!(
            c.class_key
                .starts_with("permission-prompt-command-sends-token"),
            "{c:?}"
        );
        assert!(!c.class_key.contains("zzsecret77"), "{c:?}");
        let same = classify(
            Some(&format!(
                "Command sends TOKEN={} to a remote host",
                "other999"
            )),
            None,
            None,
        );
        assert_eq!(c.class_key, same.class_key, "the hash is of redacted text");
    }

    #[test]
    fn key_tokens_and_permission_modes_are_capped() {
        let long_tool = "T".repeat(200);
        let c = classify(
            None,
            Some(&long_tool),
            Some(&format!("{} x", "c".repeat(200))),
        );
        for tok in c
            .class_key
            .strip_prefix("permission-prompt-")
            .unwrap()
            .split('-')
        {
            assert!(tok.len() <= MAX_KEY_TOKEN_CHARS, "{}", c.class_key);
        }
        let huge = "y ".repeat(MAX_CLASSIFY_INPUT_BYTES);
        let _ = classify(Some(&huge), None, Some(&huge)); // bounded, no panic

        let mut agg = Aggregator::new();
        let now = utc("2026-10-14T10:00:00Z");
        let k = classify(None, Some("Bash"), Some("rm x"));
        for i in 0..20 {
            let mode = format!("mode{i}{}", "m".repeat(100));
            agg.record(obs(&k, &mode, "rm x"), now);
        }
        let b = &agg.snapshot()[0];
        assert_eq!(b.permission_modes.len(), MAX_PERMISSION_MODES);
        assert!(b
            .permission_modes
            .iter()
            .all(|m| m.chars().count() <= MAX_PERMISSION_MODE_CHARS));
    }

    /// IF Claude Code repeats the Notification while a dialog stays open
    /// (unmeasured), a dialog open for two minutes is still ONE prompt.
    #[test]
    fn a_long_open_dialog_counts_once() {
        let mut agg = Aggregator::new();
        let t0 = utc("2026-10-14T10:00:00Z");
        assert!(agg
            .observe("tl", pr("rm -f $f", serde_json::json!([])), t0)
            .counted());
        for s in (6..=120).step_by(6) {
            let o = agg.observe(
                "tl",
                note("Claude needs your permission to use Bash"),
                t0 + chrono::Duration::seconds(s),
            );
            assert!(
                matches!(o, ObserveOutcome::Deduplicated(_)),
                "at {s}s: {o:?}"
            );
        }
        let total: u64 = agg.snapshot().iter().map(|b| b.count).sum();
        assert_eq!(total, 1);
    }

    /// Today's fleet intake is Notification-only: a dialog whose Notification
    /// repeats must still count once, and a later dialog counts again.
    #[test]
    fn a_repeating_notification_without_a_permission_request_counts_once() {
        let mut agg = Aggregator::new();
        let t0 = utc("2026-10-14T10:00:00Z");
        let at = |s: i64| t0 + chrono::Duration::seconds(s);
        let n = || note("Claude needs your permission to use Bash");
        assert!(agg.observe("tn", n(), t0).counted());
        for s in (6..=120).step_by(6) {
            let o = agg.observe("tn", n(), at(s));
            assert!(
                matches!(o, ObserveOutcome::Deduplicated(_)),
                "at {s}s: {o:?}"
            );
        }
        assert!(agg
            .observe("tn", n(), at(120 + DEDUP_WINDOW_SECS + 1))
            .counted());
        // A PermissionRequest always counts, even right after a Notification.
        assert!(agg
            .observe("tn", pr("rm x", serde_json::json!([])), at(200))
            .counted());
        let total: u64 = agg.snapshot().iter().map(|b| b.count).sum();
        assert_eq!(total, 3);
    }

    #[test]
    fn a_credential_in_the_command_word_never_reaches_the_class_key() {
        for cmd in [
            format!("https://u:{}@h.example/x", "zzpw77"),
            format!("postgres://u:{}@h/db --list", "zzpw77"),
            format!("eyJ{}.eyJ{}.zzpw77sig", "hbGciOiJIUzI1NiJ9", "zdWIiOiJ4In0"),
        ] {
            let c = classify(None, Some("Bash"), Some(&cmd));
            assert!(!c.class_key.contains("zzpw77"), "{cmd} -> {c:?}");
            assert!(!c.class_key.contains("hbgcioijiuzi1nij9"), "{cmd} -> {c:?}");
        }
    }

    #[test]
    fn a_long_cwd_is_capped() {
        let cwd = format!("/a/{}", "d".repeat(1000));
        let ex = redact_example(Some("ls"), Some(&cwd));
        assert_eq!(ex.cwd.unwrap().chars().count(), CWD_EXAMPLE_MAX_CHARS + 1);
    }

    #[test]
    fn aggregator_bounds_the_number_of_buckets() {
        let mut agg = Aggregator::new();
        let now = utc("2026-10-14T10:00:00Z");
        for i in 0..(MAX_BUCKETS + 5) {
            let c = classify(None, Some("Bash"), Some(&format!("cmd{i}")));
            agg.record(obs(&c, "default", "x"), now);
        }
        assert_eq!(agg.snapshot().len(), MAX_BUCKETS);
        assert_eq!(agg.dropped(), 5);
    }

    #[test]
    fn pasted_text_is_redacted_and_capped() {
        let pasted = format!(
            "Dangerous rm operation: API_KEY={} rm -f $f via https://{}:{}@h/x",
            "sk-1", "u", "pw"
        );
        let out = redact_pasted(&pasted);
        assert!(!out.contains("sk-1") && !out.contains("pw"), "{out}");
        assert!(out.contains("API_KEY=***"), "{out}");
        assert!(out.contains("https://***@h/x"), "{out}");
        let long = redact_pasted(&"y".repeat(10_000));
        assert_eq!(long.chars().count(), PASTED_TEXT_MAX_CHARS + 1);
    }

    #[test]
    fn the_finding_body_leads_with_its_count() {
        let mut agg = Aggregator::new();
        let c = classify(None, Some("Bash"), Some("rm -f x"));
        agg.record(
            obs(&c, "bypassPermissions", "rm -f x"),
            utc("2026-10-14T10:05:00Z"),
        );
        agg.record(obs(&c, "default", "rm -f y"), utc("2026-10-14T10:06:00Z"));
        let body = agg.snapshot()[0].render_finding_body();
        assert_eq!(body.lines().next(), Some("count: 2"), "{body}");
        assert!(body.contains("class: permission-prompt-bash-rm"), "{body}");
        assert!(
            body.contains("permission_modes: bypassPermissions, default"),
            "{body}"
        );
        assert!(body.contains("command=rm -f y"), "{body}");
    }

    #[test]
    fn stored_reason_is_redacted() {
        let mut agg = Aggregator::new();
        let c = classify(
            Some(&format!(
                "Suspicious token use in command: TOKEN={}",
                "ghp_secret123"
            )),
            None,
            None,
        );
        agg.record(
            Observation::new(c, PayloadKind::Notification, None, None, None, None),
            utc("2026-10-14T10:00:00Z"),
        );
        let b = &agg.snapshot()[0];
        assert!(
            !b.reason.as_deref().unwrap().contains("ghp_secret123"),
            "{b:?}"
        );
        assert!(
            b.examples.is_empty(),
            "an example with no content is not kept"
        );
    }

    // ---- intake ------------------------------------------------------------

    #[test]
    fn only_permission_prompt_payloads_are_parsed() {
        let idle = serde_json::json!({
            "hook_event_name": "Notification",
            "notification_type": "idle_prompt",
            "message": "Claude is waiting for your input",
        });
        assert!(parse_payload(&idle).is_none());
        let prompt = serde_json::json!({
            "hook_event_name": "Notification",
            "notification_type": "permission_prompt",
            "message": "Claude needs your permission to use Bash",
            "cwd": "/home/u/ws/repo",
            "session_id": "s",
        });
        let o = parse_payload(&prompt).unwrap();
        assert_eq!(o.class.class_key, "permission-prompt-bash");
        let req = serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": {"command": "rm -rf $X"},
            "permission_mode": "bypassPermissions",
        });
        let o = parse_payload(&req).unwrap();
        assert_eq!(o.class.class_key, "permission-prompt-bash-rm");
        assert_eq!(o.permission_mode.as_deref(), Some("bypassPermissions"));
    }

    #[test]
    fn garbage_bodies_record_nothing_and_never_panic() {
        for body in [
            &b""[..],
            b"not json",
            b"[]",
            b"null",
            b"{\"notification_type\":7}",
        ] {
            assert_eq!(observe_notification("t", body), ObserveOutcome::Ignored);
        }
    }

    fn pr(cmd: &str, suggestions: serde_json::Value) -> Observation {
        parse_payload(&serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": {"command": cmd},
            "permission_mode": "bypassPermissions",
            "permission_suggestions": suggestions,
        }))
        .unwrap()
    }

    fn note(message: &str) -> Observation {
        parse_payload(&serde_json::json!({
            "hook_event_name": "Notification",
            "notification_type": "permission_prompt",
            "message": message,
        }))
        .unwrap()
    }

    #[test]
    fn permission_suggestions_mark_the_safety_class() {
        assert_eq!(
            pr("rm -f $f", serde_json::json!([])).safety_class,
            Some(true)
        );
        assert_eq!(
            pr("rm -f $f", serde_json::json!([{"type": "addRules"}])).safety_class,
            Some(false)
        );
        assert_eq!(
            note("Claude needs your permission to use Bash").safety_class,
            None
        );

        let mut agg = Aggregator::new();
        let now = utc("2026-10-14T10:00:00Z");
        agg.record(pr("rm a", serde_json::json!([])), now);
        agg.record(pr("rm b", serde_json::json!([])), now);
        let b = &agg.snapshot()[0];
        assert_eq!((b.safety_asks, b.ordinary_asks), (2, 0));
        assert_eq!(b.safety_class(), Some(true));
        assert_eq!(b.to_json()["safety_class"], true);
        agg.record(pr("rm c", serde_json::json!([{"x": 1}])), now);
        assert_eq!(
            agg.snapshot()[0].safety_class(),
            None,
            "mixed is not a verdict"
        );
    }

    #[test]
    fn a_notification_without_a_type_but_with_the_stock_message_counts() {
        let o = parse_payload(&serde_json::json!({
            "message": "Claude needs your permission",
        }))
        .unwrap();
        assert_eq!(o.kind, PayloadKind::Notification);
        assert_eq!(o.class.class_key, "permission-prompt-unknown");
        assert!(parse_payload(&serde_json::json!({"message": "hello"})).is_none());
    }

    /// One dialog yields a PermissionRequest and (Phase 0) a Notification ~6 s
    /// later: count it once.
    #[test]
    fn the_notification_echo_of_a_permission_request_is_not_counted_twice() {
        let mut agg = Aggregator::new();
        let t0 = utc("2026-10-14T10:00:00Z");
        let at = |secs: i64| t0 + chrono::Duration::seconds(secs);
        let notification = || note("Claude needs your permission to use Bash");

        assert!(agg
            .observe("t1", pr("rm -f $f", serde_json::json!([])), t0)
            .counted());
        let echo = agg.observe("t1", notification(), at(6));
        assert!(matches!(echo, ObserveOutcome::Deduplicated(_)), "{echo:?}");
        // Another terminal's Notification is its own dialog.
        assert!(agg.observe("t2", notification(), at(6)).counted());
        // Past the window — measured from the LAST echo, which slides it — the
        // same terminal's Notification is a new dialog.
        assert!(agg
            .observe("t1", notification(), at(6 + DEDUP_WINDOW_SECS + 1))
            .counted());

        let total: u64 = agg.snapshot().iter().map(|b| b.count).sum();
        assert_eq!(total, 3);
        let rm = agg
            .snapshot()
            .into_iter()
            .find(|b| b.class_key == "permission-prompt-bash-rm")
            .unwrap();
        assert_eq!(rm.count, 1);
    }

    #[test]
    fn the_dedup_table_is_pruned() {
        let mut agg = Aggregator::new();
        let t0 = utc("2026-10-14T10:00:00Z");
        for i in 0..100 {
            agg.observe(&format!("t{i}"), pr("rm x", serde_json::json!([])), t0);
        }
        assert_eq!(agg.last_signal.len(), 100);
        let later = t0 + chrono::Duration::seconds(DEDUP_WINDOW_SECS * 2);
        agg.observe("fresh", pr("rm x", serde_json::json!([])), later);
        assert_eq!(agg.last_signal.len(), 1);
    }
}
