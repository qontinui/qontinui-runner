//! Shared PTY-output scanning primitives.
//!
//! Byte-level ANSI stripping and text normalization used by the terminal
//! output hooks ([`super::usage_limit`] and [`super::auto_response`]). Both
//! hooks watch every PTY's output for patterns split arbitrarily across read
//! chunks, so they need the same stateful escape-sequence stripper and the
//! same lowercase/whitespace-collapse normalization. Those primitives live
//! here so neither hook owns the other.
//!
//! Hot-path discipline: these run on the PTY reader thread for every chunk —
//! one pass of a byte-level state machine plus a bounded normalization, no
//! regex, no locks.

/// Stripped-text rolling window kept per terminal. Must comfortably exceed
/// the longest pattern so a phrase split across two PTY chunks still matches
/// after the second chunk arrives.
pub(crate) const WINDOW_KEEP: usize = 256;

// ── ANSI stripping ──────────────────────────────────────────────────────────

/// Byte-level escape-sequence stripper, stateful so sequences split across
/// PTY read chunks are still consumed (a split CSI must not leak fragments
/// into the text window between two words of a matched message).
#[derive(Default)]
pub(crate) struct AnsiStripper {
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
    pub(crate) fn feed(&mut self, data: &[u8], out: &mut String) {
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

// ── Normalization ────────────────────────────────────────────────────────────

/// Lowercase + collapse whitespace runs to single spaces, so TUI padding /
/// line wraps inside a message don't break the substring (or regex) match.
pub(crate) fn normalize(window: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_lowercases_and_collapses_whitespace() {
        assert_eq!(normalize("USAGE   LIMIT\n REACHED"), "usage limit reached");
        assert_eq!(normalize("  Hello\t\tWorld  "), " hello world ");
    }

    #[test]
    fn ansi_stripper_consumes_csi_split_across_feeds() {
        let mut s = AnsiStripper::default();
        let mut out = String::new();
        // CSI starts at the end of feed 1 and finishes at the start of feed 2 —
        // the fragment must not leak into the text window.
        s.feed(b"usage \x1b[38;5;", &mut out);
        s.feed(b"196mlimit", &mut out);
        assert_eq!(out, "usage limit");
    }
}
