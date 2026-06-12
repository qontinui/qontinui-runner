//! Usage-limit detection over raw terminal PTY output.
//!
//! [`UsageLimitWatchHook`] is an [`OutputHook`] installed on the terminal
//! manager's interceptor pipeline. It watches every PTY's output stream for
//! the Claude CLI's token-exhaustion messages ("Claude usage limit reached",
//! "5-hour limit reached ∙ resets …", …) and fires a callback hint when one
//! appears.
//!
//! The hint is deliberately a HINT, not a verdict: conversation text can echo
//! these phrases (a diff of this very file, a quoted error in a log review…),
//! and a TUI repaint after `claude --resume` can re-render a *historical*
//! limit message. The downstream handler
//! (`terminal::account_migration::handle_usage_limit_hint`) therefore
//! re-probes the account's real usage and only acts when the probe confirms
//! exhaustion. This module only has to be cheap, tolerant, and debounced.
//!
//! Hot-path discipline: `process` runs on the PTY reader thread for every
//! chunk. Work per chunk is one pass of a byte-level ANSI-strip state machine
//! plus a bounded substring scan over a small rolling window — no regex, no
//! allocation growth, no locks held across the callback (the callback is
//! invoked after the state lock is released).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{debug, info};

use super::interceptor::OutputHook;

/// Lowercased substrings that indicate the Claude CLI hit a usage limit.
///
/// Matched against an ANSI-stripped, lowercased, whitespace-collapsed rolling
/// window of recent output. Generous on purpose — a false hint costs one
/// (rate-limited) usage probe downstream, while a missed hint strands a
/// session on a dead account.
const USAGE_LIMIT_PATTERNS: &[&str] = &[
    "usage limit reached",
    "5-hour limit reached",
    "weekly limit reached",
    "hit your usage limit",
    "out of extra usage",
    "session limit reached",
];

/// Minimum gap between two hint firings for the same terminal. A limit
/// message that keeps re-rendering (TUI repaints on resize/scroll) must not
/// hammer the probe path.
const HINT_DEBOUNCE: Duration = Duration::from_secs(300);

/// Stripped-text rolling window kept per terminal. Must comfortably exceed
/// the longest pattern so a phrase split across two PTY chunks still matches
/// after the second chunk arrives.
const WINDOW_KEEP: usize = 256;

// ── ANSI stripping ──────────────────────────────────────────────────────────

/// Byte-level escape-sequence stripper, stateful so sequences split across
/// PTY read chunks are still consumed (a split CSI must not leak fragments
/// into the text window between two words of a limit message).
#[derive(Default)]
struct AnsiStripper {
    state: StripState,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum StripState {
    #[default]
    Ground,
    /// Saw ESC, awaiting the introducer byte.
    Escape,
    /// Inside `ESC [ … <final>` — consume until a final byte `0x40..=0x7E`.
    Csi,
    /// Inside `ESC ] … (BEL | ESC \)` — consume until the terminator.
    Osc,
    /// Inside an OSC, saw ESC — next `\` ends the OSC.
    OscEscape,
}

impl AnsiStripper {
    /// Feed raw PTY bytes; append printable text (with control bytes mapped
    /// to single spaces) onto `out`.
    fn feed(&mut self, data: &[u8], out: &mut String) {
        for &b in data {
            match self.state {
                StripState::Ground => match b {
                    0x1b => self.state = StripState::Escape,
                    b'\r' | b'\n' | b'\t' => out.push(' '),
                    0x00..=0x1f | 0x7f => {} // other control bytes — drop
                    _ => out.push(b as char), // lossy-ASCII: multibyte UTF-8
                                              // bytes land as garbage chars,
                                              // which never match the ASCII
                                              // patterns and are trimmed away
                                              // by the rolling window.
                },
                StripState::Escape => match b {
                    b'[' => self.state = StripState::Csi,
                    b']' => self.state = StripState::Osc,
                    // Two-byte sequences (ESC + one char) and anything
                    // unrecognized: swallow and return to ground.
                    _ => self.state = StripState::Ground,
                },
                StripState::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        self.state = StripState::Ground;
                    }
                }
                StripState::Osc => match b {
                    0x07 => self.state = StripState::Ground,
                    0x1b => self.state = StripState::OscEscape,
                    _ => {}
                },
                StripState::OscEscape => {
                    self.state = if b == b'\\' {
                        StripState::Ground
                    } else {
                        StripState::Osc
                    };
                }
            }
        }
    }
}

// ── Pattern matching ────────────────────────────────────────────────────────

/// Lowercase + collapse whitespace runs to single spaces, so TUI padding /
/// line wraps inside a message don't break the substring match.
fn normalize(window: &str) -> String {
    let mut out = String::with_capacity(window.len());
    let mut last_space = false;
    for c in window.chars() {
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            last_space = false;
        }
    }
    out
}

/// First matching usage-limit pattern in the (raw, un-normalized) window,
/// if any.
pub fn window_indicates_usage_limit(window: &str) -> Option<&'static str> {
    let normalized = normalize(window);
    USAGE_LIMIT_PATTERNS
        .iter()
        .find(|p| normalized.contains(*p))
        .copied()
}

// ── The hook ────────────────────────────────────────────────────────────────

struct TermWatchState {
    stripper: AnsiStripper,
    window: String,
    last_fired: Option<Instant>,
}

impl Default for TermWatchState {
    fn default() -> Self {
        Self {
            stripper: AnsiStripper::default(),
            window: String::with_capacity(WINDOW_KEEP * 2),
            last_fired: None,
        }
    }
}

/// Callback type: `(terminal_id, matched_pattern)`.
pub type UsageLimitHintFn = Box<dyn Fn(String, &'static str) + Send + Sync>;

/// Output hook that fires `on_hint` (debounced per terminal) whenever a
/// usage-limit message appears in a PTY's output. Pass-through for the data
/// itself — it never modifies the stream.
pub struct UsageLimitWatchHook {
    states: Mutex<HashMap<String, TermWatchState>>,
    on_hint: UsageLimitHintFn,
    debounce: Duration,
}

impl UsageLimitWatchHook {
    pub fn new(on_hint: UsageLimitHintFn) -> Self {
        Self::with_debounce(on_hint, HINT_DEBOUNCE)
    }

    fn with_debounce(on_hint: UsageLimitHintFn, debounce: Duration) -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
            on_hint,
            debounce,
        }
    }

    /// Feed a chunk for `terminal_id`; returns the matched pattern when a
    /// (debounce-permitted) hint should fire. Split out from
    /// [`OutputHook::process`] so tests can drive chunking/debounce without a
    /// callback.
    fn scan(&self, terminal_id: &str, data: &[u8]) -> Option<&'static str> {
        let mut states = self.states.lock().ok()?;
        let st = states.entry(terminal_id.to_string()).or_default();

        st.stripper.feed(data, &mut st.window);

        let matched = window_indicates_usage_limit(&st.window);

        // Trim the rolling window AFTER matching. On a match, clear it
        // entirely so the same rendered message doesn't re-match on the next
        // chunk (the debounce also guards this, but a cleared window keeps
        // the state machine honest).
        if matched.is_some() {
            st.window.clear();
        } else if st.window.len() > WINDOW_KEEP {
            let cut = st.window.len() - WINDOW_KEEP;
            // Avoid splitting a UTF-8 char (window may hold lossy bytes but
            // `normalize` already tolerates garbage; keep it valid anyway).
            let cut = (cut..st.window.len())
                .find(|i| st.window.is_char_boundary(*i))
                .unwrap_or(0);
            st.window.drain(..cut);
        }

        let pattern = matched?;
        let now = Instant::now();
        if let Some(prev) = st.last_fired {
            if now.duration_since(prev) < self.debounce {
                debug!(
                    terminal_id,
                    pattern, "usage-limit message re-seen within debounce — suppressed"
                );
                return None;
            }
        }
        st.last_fired = Some(now);
        Some(pattern)
    }

    /// Drop per-terminal state for a closed terminal (best-effort hygiene;
    /// stale entries are bounded by terminal count anyway).
    #[allow(dead_code)]
    pub fn forget(&self, terminal_id: &str) {
        if let Ok(mut states) = self.states.lock() {
            states.remove(terminal_id);
        }
    }
}

impl OutputHook for UsageLimitWatchHook {
    fn process(&self, terminal_id: &str, data: &[u8]) -> Vec<u8> {
        if let Some(pattern) = self.scan(terminal_id, data) {
            info!(
                terminal_id,
                pattern, "usage-limit message detected in terminal output"
            );
            (self.on_hint)(terminal_id.to_string(), pattern);
        }
        data.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook() -> UsageLimitWatchHook {
        UsageLimitWatchHook::with_debounce(Box::new(|_, _| {}), Duration::from_secs(300))
    }

    #[test]
    fn plain_limit_message_matches() {
        assert_eq!(
            window_indicates_usage_limit(
                "Claude usage limit reached. Your limit will reset at 3am."
            ),
            Some("usage limit reached")
        );
        assert_eq!(
            window_indicates_usage_limit("5-hour limit reached ∙ resets 3am"),
            Some("5-hour limit reached")
        );
        assert_eq!(
            window_indicates_usage_limit("Weekly limit reached — resets Thursday"),
            Some("weekly limit reached")
        );
    }

    #[test]
    fn case_and_whitespace_tolerant() {
        assert_eq!(
            window_indicates_usage_limit("USAGE   LIMIT\n REACHED"),
            Some("usage limit reached")
        );
    }

    #[test]
    fn ordinary_output_does_not_match() {
        assert_eq!(
            window_indicates_usage_limit("cargo build finished in 12s"),
            None
        );
        assert_eq!(
            window_indicates_usage_limit("approaching usage limit"),
            None
        );
    }

    #[test]
    fn ansi_codes_inside_message_are_stripped() {
        let h = hook();
        // Color change mid-phrase must not break the match.
        let chunk = b"\x1b[31musage \x1b[1mlimit\x1b[0m reached\x1b[0m";
        assert_eq!(h.scan("t1", chunk), Some("usage limit reached"));
    }

    #[test]
    fn message_split_across_chunks_matches_on_second_chunk() {
        let h = hook();
        assert_eq!(h.scan("t1", b"Claude usage li"), None);
        assert_eq!(h.scan("t1", b"mit reached."), Some("usage limit reached"));
    }

    #[test]
    fn escape_sequence_split_across_chunks_is_consumed() {
        let h = hook();
        // CSI starts at the end of chunk 1 and finishes at the start of
        // chunk 2 — the fragment must not pollute the text window.
        assert_eq!(h.scan("t1", b"usage \x1b[38;5;"), None);
        assert_eq!(
            h.scan("t1", b"196mlimit reached"),
            Some("usage limit reached")
        );
    }

    #[test]
    fn osc_title_sequence_is_stripped() {
        let h = hook();
        let chunk = b"\x1b]0;my terminal title\x07usage limit reached";
        assert_eq!(h.scan("t1", chunk), Some("usage limit reached"));
    }

    #[test]
    fn debounce_suppresses_repeat_within_window() {
        let h = hook();
        assert_eq!(
            h.scan("t1", b"usage limit reached"),
            Some("usage limit reached")
        );
        // Re-rendered message (e.g. TUI repaint) within the debounce window.
        assert_eq!(h.scan("t1", b"usage limit reached"), None);
    }

    #[test]
    fn debounce_is_per_terminal() {
        let h = hook();
        assert_eq!(
            h.scan("t1", b"usage limit reached"),
            Some("usage limit reached")
        );
        assert_eq!(
            h.scan("t2", b"usage limit reached"),
            Some("usage limit reached")
        );
    }

    #[test]
    fn window_is_bounded() {
        let h = hook();
        // Feed lots of non-matching output; window must stay bounded and a
        // later message must still match.
        for _ in 0..100 {
            assert_eq!(h.scan("t1", &[b'x'; 1024]), None);
        }
        {
            let states = h.states.lock().unwrap();
            let st = states.get("t1").unwrap();
            assert!(st.window.len() <= WINDOW_KEEP + 1024);
        }
        assert_eq!(
            h.scan("t1", b" weekly limit reached"),
            Some("weekly limit reached")
        );
    }

    #[test]
    fn passthrough_returns_data_unchanged() {
        let h = UsageLimitWatchHook::new(Box::new(|_, _| {}));
        let data = b"\x1b[31musage limit reached\x1b[0m".to_vec();
        let out = OutputHook::process(&h, "t1", &data);
        assert_eq!(out, data);
    }
}
