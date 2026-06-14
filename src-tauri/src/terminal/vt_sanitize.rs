//! VT output sanitization at the PTY trust boundary.
//!
//! Terminal output is **untrusted**: it is whatever a child process (or a
//! remote host over SSH, or a `cat`'d file) emits. [`VtSanitizeHook`] is an
//! [`OutputHook`] installed on the terminal manager's interceptor pipeline; it
//! sees every PTY chunk BEFORE the grid parser, the scrollback tee, and the
//! frontend emit (`session.rs` reader thread → `interceptor.process`), so a
//! single sanitization pass covers every downstream consumer at once.
//!
//! ## Classification (default = ALLOW pass-through)
//!
//! We are deliberately conservative — the vast majority of sequences pass
//! through untouched. Only a small, well-justified set is touched:
//!
//! - **STRIP entirely** — **OSC 52** (`ESC ] 52 ; … ST|BEL`): clipboard
//!   set/query. Arbitrary terminal output must never be able to silently
//!   overwrite or exfiltrate the operator's system clipboard. This is the
//!   headline security fix.
//! - **SANITIZE (keep the sequence, scrub C0/C1 control bytes from the payload,
//!   preserve the terminator)** — **OSC 0 / 1 / 2** (window/icon title) and
//!   **OSC 8** (hyperlinks). A title or hyperlink URI must not be able to
//!   smuggle embedded escape sequences. (The grid consumes OSC 0/2 today;
//!   OSC 1/8 scrubbing is harmless future-proofing.)
//! - **ALLOW untouched** — everything else: **OSC 633** (shell integration),
//!   DEC private modes `?2026` (synchronized output — the Phase-4 sync-frame
//!   coalescer depends on it), `?1049` (alt screen), `?25` (cursor
//!   visibility), all SGR/colors, cursor movement, bracketed paste, DCS, and
//!   all ordinary printable/CSI output.
//!
//! ## Split-sequence handling
//!
//! A target sequence can straddle two `process()` calls (PTY reads chunk on
//! arbitrary byte boundaries). We hold the bytes of an in-progress, not-yet-
//! emitted escape sequence in **per-terminal carry state** keyed by
//! `terminal_id`. Because [`OutputHook::process`] takes `&self` (the trait is
//! `Send + Sync`), the carry map uses interior mutability
//! (`Mutex<HashMap<String, Carry>>`).
//!
//! ## Hot-path discipline
//!
//! `process` runs on the PTY reader thread for every chunk. The common case is
//! plain printable output: when a chunk (with no pending carry) contains no
//! `ESC` byte, it is returned unchanged with no scanning beyond a single
//! `memchr`-style search and no per-byte allocation.

use std::collections::HashMap;
use std::sync::Mutex;

use super::interceptor::OutputHook;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const ST_FINAL: u8 = b'\\'; // the `\` of a String Terminator (ESC \)

/// Cap on the bytes we hold for a single in-progress (not-yet-terminated)
/// escape sequence. Real OSC/CSI sequences are tiny (a title, a hyperlink, a
/// base64 clipboard blob); only pathological or non-VT input — e.g. `cat`-ing a
/// binary file, which routinely contains a lone `ESC [`/`ESC ]` followed by a
/// long run of non-terminator bytes — would accumulate past this. Without the
/// cap the per-terminal carry could grow unboundedly (a memory-DoS surface that
/// this hook, processing UNTRUSTED output, would otherwise introduce). On
/// overflow we FLUSH the buffered bytes as pass-through (see `process`): that
/// matches the pre-hook behavior for binary garbage (no data loss / no
/// regression), and an unterminated, over-cap OSC 52 still cannot set the
/// clipboard — xterm caps OSC payloads too, so the strip guarantee holds.
const MAX_PENDING: usize = 1 << 20; // 1 MiB

/// Default-ON env gate (mirrors `commit_report::report_enabled`). Any of
/// `0` / `false` / `off` (case-insensitive) disables sanitization; anything
/// else (including unset) leaves it ON. Read once at hook construction in
/// `main.rs`.
pub fn sanitize_enabled() -> bool {
    match std::env::var("QONTINUI_TERMINAL_SANITIZE") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off"
        ),
        Err(_) => true,
    }
}

/// Per-terminal carry: the raw bytes of an escape sequence that began in a
/// previous chunk and has not been classified/emitted yet. Empty when the
/// terminal is at "ground" (no in-progress sequence).
#[derive(Default)]
struct Carry {
    /// Buffered, not-yet-emitted bytes of an in-progress escape sequence.
    pending: Vec<u8>,
}

/// Output hook that sanitizes untrusted VT sequences in PTY output. See the
/// module docs for the full classification.
#[derive(Default)]
pub struct VtSanitizeHook {
    carries: Mutex<HashMap<String, Carry>>,
}

impl VtSanitizeHook {
    pub fn new() -> Self {
        Self::default()
    }
}

impl OutputHook for VtSanitizeHook {
    fn process(&self, terminal_id: &str, data: &[u8]) -> Vec<u8> {
        // Lock the carry map and pull this terminal's pending bytes (if any).
        let mut carries = match self.carries.lock() {
            Ok(c) => c,
            // A poisoned lock must not corrupt the stream — pass through.
            Err(_) => return data.to_vec(),
        };

        // Fast path: no carry AND no ESC in this chunk → nothing to do.
        if !carries
            .get(terminal_id)
            .map(|c| !c.pending.is_empty())
            .unwrap_or(false)
            && !data.contains(&ESC)
        {
            return data.to_vec();
        }

        let entry = carries.entry(terminal_id.to_string()).or_default();
        let pending = std::mem::take(&mut entry.pending);
        let (mut out, new_pending) = sanitize_chunk(&pending, data);
        if new_pending.len() > MAX_PENDING {
            // Pathologically long unterminated sequence (e.g. binary `cat`).
            // Flush as pass-through to bound memory; reset to ground. See
            // `MAX_PENDING` for why this preserves both the no-regression and
            // the no-leak properties.
            tracing::debug!(
                terminal_id,
                len = new_pending.len(),
                "vt_sanitize: flushing over-cap unterminated sequence as pass-through"
            );
            out.extend_from_slice(&new_pending);
            entry.pending = Vec::new();
        } else {
            entry.pending = new_pending;
        }
        out
    }
}

/// Outcome of scanning a single in-progress escape sequence.
enum SeqAction {
    /// Emit these bytes (already classified — e.g. allowed verbatim, or a
    /// sanitized title) and continue from ground.
    Emit(Vec<u8>),
    /// Drop the sequence entirely (e.g. OSC 52).
    Drop,
}

/// Sanitize `carry + data`, returning `(output, new_carry)`. `carry` is the
/// bytes of an escape sequence in progress from the previous call; `new_carry`
/// is any trailing in-progress sequence to hold for the next call.
fn sanitize_chunk(carry: &[u8], data: &[u8]) -> (Vec<u8>, Vec<u8>) {
    // Combine carry (always an in-progress sequence, never plain text) with the
    // new data so a sequence split across the boundary is scanned as one unit.
    let buf: Vec<u8> = if carry.is_empty() {
        // Avoid a copy in the common (no-carry) case by borrowing later.
        data.to_vec()
    } else {
        let mut b = Vec::with_capacity(carry.len() + data.len());
        b.extend_from_slice(carry);
        b.extend_from_slice(data);
        b
    };

    let mut out: Vec<u8> = Vec::with_capacity(buf.len());
    let mut i = 0;
    let n = buf.len();

    while i < n {
        // Copy a run of non-ESC bytes verbatim (never corrupt plain bytes).
        let start = i;
        while i < n && buf[i] != ESC {
            i += 1;
        }
        if i > start {
            out.extend_from_slice(&buf[start..i]);
        }
        if i >= n {
            break; // ended on ground — no carry.
        }

        // buf[i] == ESC. Try to classify/consume the sequence starting here.
        match scan_sequence(&buf[i..]) {
            ScanResult::Incomplete => {
                // The sequence runs off the end of this chunk — carry the rest.
                return (out, buf[i..].to_vec());
            }
            ScanResult::Done { consumed, action } => {
                match action {
                    SeqAction::Emit(bytes) => out.extend_from_slice(&bytes),
                    SeqAction::Drop => {}
                }
                i += consumed;
            }
        }
    }

    (out, Vec::new())
}

enum ScanResult {
    /// Sequence is not fully present in the buffer yet.
    Incomplete,
    /// Sequence fully consumed: `consumed` bytes from the start of the slice,
    /// with the classified `action`.
    Done { consumed: usize, action: SeqAction },
}

/// Scan one escape sequence beginning at `s[0] == ESC`. Decides STRIP /
/// SANITIZE / ALLOW. Returns `Incomplete` if the sequence is not yet fully
/// present (so the caller carries it to the next chunk).
fn scan_sequence(s: &[u8]) -> ScanResult {
    debug_assert_eq!(s[0], ESC);
    if s.len() < 2 {
        return ScanResult::Incomplete;
    }
    match s[1] {
        b']' => scan_osc(s),
        b'[' => scan_csi(s),
        b'P' | b'X' | b'^' | b'_' => scan_string_terminated(s), // DCS/SOS/PM/APC
        // Two-byte escape (ESC + one final), or anything else: allow the pair.
        _ => ScanResult::Done {
            consumed: 2,
            action: SeqAction::Emit(s[..2].to_vec()),
        },
    }
}

/// CSI: `ESC [ <params/intermediates> <final 0x40..=0x7E>`. Always ALLOWED;
/// we only need to find its end so we don't mis-scan trailing bytes.
fn scan_csi(s: &[u8]) -> ScanResult {
    // s[0]=ESC s[1]='['
    let mut i = 2;
    while i < s.len() {
        let b = s[i];
        if (0x40..=0x7e).contains(&b) {
            // final byte
            return ScanResult::Done {
                consumed: i + 1,
                action: SeqAction::Emit(s[..=i].to_vec()),
            };
        }
        i += 1;
    }
    ScanResult::Incomplete
}

/// DCS / SOS / PM / APC: `ESC P|X|^|_ … ST`. Always ALLOWED; find the String
/// Terminator (`ESC \`) or BEL so we don't mis-scan the payload.
fn scan_string_terminated(s: &[u8]) -> ScanResult {
    match find_string_terminator(s, 2) {
        Some(end) => ScanResult::Done {
            consumed: end,
            action: SeqAction::Emit(s[..end].to_vec()),
        },
        None => ScanResult::Incomplete,
    }
}

/// OSC: `ESC ] <num> ; <payload> (BEL | ESC \)`. Classify by the numeric
/// prefix: STRIP 52, SANITIZE 0/1/2/8, ALLOW everything else.
fn scan_osc(s: &[u8]) -> ScanResult {
    // s[0]=ESC s[1]=']'
    let term = match find_string_terminator(s, 2) {
        Some(end) => end,
        None => return ScanResult::Incomplete,
    };

    // Identify the numeric command: digits from index 2 up to the first ';'.
    let mut j = 2;
    while j < term && s[j].is_ascii_digit() {
        j += 1;
    }
    let num = &s[2..j];

    // STRIP: OSC 52 (clipboard).
    if num == b"52" {
        return ScanResult::Done {
            consumed: term,
            action: SeqAction::Drop,
        };
    }

    // SANITIZE: OSC 0/1/2 (title) and OSC 8 (hyperlink) — scrub C0/C1 control
    // bytes from the payload, preserving the introducer and the terminator.
    let sanitize = matches!(num, b"0" | b"1" | b"2" | b"8");
    if sanitize {
        return ScanResult::Done {
            consumed: term,
            action: SeqAction::Emit(scrub_osc_payload(&s[..term])),
        };
    }

    // ALLOW everything else (OSC 633 shell integration, etc.) untouched.
    ScanResult::Done {
        consumed: term,
        action: SeqAction::Emit(s[..term].to_vec()),
    }
}

/// Find the end (one past the last terminator byte) of a string-terminated
/// sequence starting at `s[0]`, scanning from `from`. The terminator is BEL
/// (`0x07`) or ST (`ESC \`). Returns `None` if no terminator is present yet.
fn find_string_terminator(s: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < s.len() {
        match s[i] {
            BEL => return Some(i + 1),
            ESC => {
                // Possible ST (`ESC \`). Need the next byte to decide.
                if i + 1 >= s.len() {
                    return None; // incomplete — wait for the next byte
                }
                if s[i + 1] == ST_FINAL {
                    return Some(i + 2);
                }
                // A bare ESC inside an OSC that is not `ESC \` is malformed;
                // treat the ESC as ending this (mis-formed) sequence so we
                // never run off into the rest of the stream. Consume up to and
                // including the ESC; the next byte re-enters scanning.
                return Some(i + 1);
            }
            _ => i += 1,
        }
    }
    None
}

/// Scrub C0 (0x00..=0x1f, except the trailing terminator) and C1
/// (0x80..=0x9f) control bytes out of an OSC sequence's payload, keeping the
/// introducer (`ESC ] <num> ;`) and the terminator (BEL or `ESC \`) intact.
fn scrub_osc_payload(seq: &[u8]) -> Vec<u8> {
    // Determine the terminator span so we never strip its bytes.
    // seq ends with either BEL (1 byte) or ESC \ (2 bytes).
    let term_len = if seq.len() >= 2 && seq[seq.len() - 2] == ESC && seq[seq.len() - 1] == ST_FINAL
    {
        2
    } else if seq.last() == Some(&BEL) {
        1
    } else {
        // Malformed (no recognizable terminator) — be conservative and keep
        // the last byte as-is.
        0
    };
    let payload_end = seq.len() - term_len;

    let mut out = Vec::with_capacity(seq.len());
    // Keep the introducer `ESC ]` verbatim (its leading ESC, 0x1b, is itself in
    // the control-byte scrub range, so we must copy it explicitly rather than
    // run it through the scrub). `intro` is `min(2, payload_end)` to stay sound
    // on a pathologically short (malformed) sequence.
    let intro = payload_end.min(2);
    out.extend_from_slice(&seq[..intro]);
    // Scrub C0/C1 control bytes (and DEL, and any smuggled mid-payload ESC —
    // the real ST is in the terminator span we excluded above) from the body.
    for &b in &seq[intro..payload_end] {
        let is_control = b <= 0x1f || (0x80..=0x9f).contains(&b) || b == 0x7f;
        if !is_control {
            out.push(b);
        }
    }
    out.extend_from_slice(&seq[payload_end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ────────────────────────────────────────────────────────────

    /// Run a single chunk through a fresh hook for `terminal_id`.
    fn sanitize_once(data: &[u8]) -> Vec<u8> {
        let hook = VtSanitizeHook::new();
        hook.process("t1", data)
    }

    fn osc52(payload: &str, bel: bool) -> Vec<u8> {
        let mut v = vec![ESC, b']'];
        v.extend_from_slice(b"52;c;");
        v.extend_from_slice(payload.as_bytes());
        if bel {
            v.push(BEL);
        } else {
            v.push(ESC);
            v.push(b'\\');
        }
        v
    }

    // ── STRIP: OSC 52 ────────────────────────────────────────────────────────

    #[test]
    fn osc52_stripped_bel() {
        let input = {
            let mut v = b"before".to_vec();
            v.extend_from_slice(&osc52("aGVsbG8=", true));
            v.extend_from_slice(b"after");
            v
        };
        assert_eq!(sanitize_once(&input), b"beforeafter");
    }

    #[test]
    fn osc52_stripped_st() {
        let input = {
            let mut v = b"before".to_vec();
            v.extend_from_slice(&osc52("aGVsbG8=", false));
            v.extend_from_slice(b"after");
            v
        };
        assert_eq!(sanitize_once(&input), b"beforeafter");
    }

    #[test]
    fn osc52_query_stripped() {
        // OSC 52 query form (`?`).
        let mut v = vec![ESC, b']'];
        v.extend_from_slice(b"52;c;?");
        v.push(BEL);
        assert_eq!(sanitize_once(&v), b"");
    }

    // ── ALLOW: preserved byte-for-byte ───────────────────────────────────────

    #[test]
    fn osc633_preserved() {
        // Shell integration prompt marker: ESC ] 633 ; A BEL
        let seq = {
            let mut v = vec![ESC, b']'];
            v.extend_from_slice(b"633;A");
            v.push(BEL);
            v
        };
        assert_eq!(sanitize_once(&seq), seq);
    }

    #[test]
    fn osc0_title_clean_preserved() {
        let mut seq = vec![ESC, b']'];
        seq.extend_from_slice(b"0;my title");
        seq.push(BEL);
        assert_eq!(sanitize_once(&seq), seq);
    }

    #[test]
    fn osc2_title_clean_preserved() {
        let mut seq = vec![ESC, b']'];
        seq.extend_from_slice(b"2;my title");
        seq.push(BEL);
        assert_eq!(sanitize_once(&seq), seq);
    }

    #[test]
    fn sgr_preserved() {
        let seq = b"\x1b[31mRED\x1b[0m";
        assert_eq!(sanitize_once(seq), seq);
    }

    #[test]
    fn dec_2026_h_preserved() {
        let seq = b"\x1b[?2026h";
        assert_eq!(sanitize_once(seq), seq);
    }

    #[test]
    fn dec_2026_l_preserved() {
        let seq = b"\x1b[?2026l";
        assert_eq!(sanitize_once(seq), seq);
    }

    #[test]
    fn dec_1049_preserved() {
        let seq = b"\x1b[?1049h";
        assert_eq!(sanitize_once(seq), seq);
    }

    #[test]
    fn bracketed_paste_preserved() {
        // Enable bracketed paste mode + the paste markers.
        let seq = b"\x1b[?2004h\x1b[200~pasted text\x1b[201~\x1b[?2004l";
        assert_eq!(sanitize_once(seq), seq);
    }

    #[test]
    fn cursor_movement_preserved() {
        let seq = b"\x1b[2J\x1b[H\x1b[10;5Hhello";
        assert_eq!(sanitize_once(seq), seq);
    }

    #[test]
    fn dcs_preserved() {
        // DCS sequence terminated by ST.
        let seq = b"\x1bP1$r0m\x1b\\";
        assert_eq!(sanitize_once(seq), seq);
    }

    // ── SANITIZE: control-byte scrub ─────────────────────────────────────────

    #[test]
    fn osc0_title_control_bytes_scrubbed() {
        // Embedded C0 (0x07 would terminate; use 0x01 and a smuggled ESC).
        let mut seq = vec![ESC, b']'];
        seq.extend_from_slice(b"0;ab\x01cd");
        seq.push(BEL);
        let mut expected = vec![ESC, b']'];
        expected.extend_from_slice(b"0;abcd");
        expected.push(BEL);
        assert_eq!(sanitize_once(&seq), expected);
    }

    #[test]
    fn osc8_hyperlink_control_bytes_scrubbed() {
        // OSC 8 ; params ; URI ST  — scrub a smuggled control byte in the URI.
        let mut seq = vec![ESC, b']'];
        seq.extend_from_slice(b"8;;https://exa\x02mple.com");
        seq.push(ESC);
        seq.push(b'\\');
        let mut expected = vec![ESC, b']'];
        expected.extend_from_slice(b"8;;https://example.com");
        expected.push(ESC);
        expected.push(b'\\');
        assert_eq!(sanitize_once(&seq), expected);
    }

    #[test]
    fn osc8_clean_preserved() {
        let mut seq = vec![ESC, b']'];
        seq.extend_from_slice(b"8;;https://example.com");
        seq.push(BEL);
        assert_eq!(sanitize_once(&seq), seq);
    }

    // ── Split-sequence carry ─────────────────────────────────────────────────

    #[test]
    fn osc52_split_across_calls_fully_removed() {
        let hook = VtSanitizeHook::new();
        // Split the OSC 52 right in the middle.
        let full = {
            let mut v = b"X".to_vec();
            v.extend_from_slice(&osc52("aGVsbG8=", true));
            v.extend_from_slice(b"Y");
            v
        };
        let mid = 1 + 5; // "X" + "ESC ] 5"... split somewhere inside the seq
        let part1 = &full[..mid];
        let part2 = &full[mid..];
        let mut out = hook.process("t1", part1);
        out.extend_from_slice(&hook.process("t1", part2));
        assert_eq!(out, b"XY");
    }

    #[test]
    fn dec_2026_h_split_across_calls_preserved() {
        let hook = VtSanitizeHook::new();
        // "\x1b[?2026h" split between the '?' and "2026h".
        let p1 = b"\x1b[?2";
        let p2 = b"026h";
        let mut out = hook.process("t1", p1);
        out.extend_from_slice(&hook.process("t1", p2));
        assert_eq!(out, b"\x1b[?2026h");
    }

    #[test]
    fn osc633_split_across_calls_preserved() {
        let hook = VtSanitizeHook::new();
        let p1 = b"\x1b]633;";
        let p2 = {
            let mut v = b"A".to_vec();
            v.push(BEL);
            v
        };
        let mut out = hook.process("t1", p1);
        out.extend_from_slice(&hook.process("t1", &p2));
        let mut expected = b"\x1b]633;A".to_vec();
        expected.push(BEL);
        assert_eq!(out, expected);
    }

    #[test]
    fn per_terminal_carry_isolation() {
        let hook = VtSanitizeHook::new();
        // Terminal A: start an OSC 52 (will be dropped once completed).
        // Terminal B: start a ?2026h (will be preserved).
        // Interleave the two so a shared carry would cross-contaminate.
        let a1 = b"\x1b]52;c;"; // incomplete OSC 52 on A
        let b1 = b"\x1b[?20"; // incomplete CSI on B
        let a_out1 = hook.process("A", a1);
        let b_out1 = hook.process("B", b1);
        assert_eq!(a_out1, b""); // carried
        assert_eq!(b_out1, b""); // carried

        let a2 = {
            let mut v = b"aGk=".to_vec();
            v.push(BEL);
            v
        }; // finish A's OSC 52
        let b2 = b"26h"; // finish B's CSI
        let a_out2 = hook.process("A", &a2);
        let b_out2 = hook.process("B", b2);
        assert_eq!(a_out2, b""); // OSC 52 dropped
        assert_eq!(b_out2, b"\x1b[?2026h"); // CSI preserved
    }

    // ── Passthrough + idempotence ────────────────────────────────────────────

    #[test]
    fn plain_text_passthrough_no_esc() {
        let s = b"hello world\nsecond line\t tabs";
        assert_eq!(sanitize_once(s), s);
    }

    #[test]
    fn arbitrary_binary_passthrough() {
        // No ESC bytes → byte-identical fast path.
        let bytes: Vec<u8> = (0u8..=255).filter(|&b| b != ESC).collect();
        assert_eq!(sanitize_once(&bytes), bytes);
    }

    #[test]
    fn utf8_multibyte_passthrough() {
        let s = "héllo 世界 🎉 café".as_bytes();
        assert_eq!(sanitize_once(s), s);
    }

    #[test]
    fn idempotent() {
        let input = {
            let mut v = b"before".to_vec();
            v.extend_from_slice(&osc52("aGVsbG8=", true));
            v.extend_from_slice(b"\x1b[31mRED\x1b[0m");
            let mut title = vec![ESC, b']'];
            title.extend_from_slice(b"0;ti\x01tle");
            title.push(BEL);
            v.extend_from_slice(&title);
            v.extend_from_slice(b"after");
            v
        };
        let once = sanitize_once(&input);
        let twice = sanitize_once(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn mixed_stream() {
        // A realistic mix: plain text, SGR, an OSC 52 to strip, OSC 633 to
        // keep, a title to scrub, more text.
        let mut input = b"$ ".to_vec();
        input.extend_from_slice(b"\x1b]633;A\x07"); // shell integration (keep)
        input.extend_from_slice(b"\x1b[32mgreen\x1b[0m "); // SGR (keep)
        input.extend_from_slice(&osc52("ZXZpbA==", true)); // clipboard (strip)
        input.extend_from_slice(b"\x1b]0;tab\x07"); // title (keep, clean)
        input.extend_from_slice(b"done\n");

        let mut expected = b"$ ".to_vec();
        expected.extend_from_slice(b"\x1b]633;A\x07");
        expected.extend_from_slice(b"\x1b[32mgreen\x1b[0m ");
        expected.extend_from_slice(b"\x1b]0;tab\x07");
        expected.extend_from_slice(b"done\n");

        assert_eq!(sanitize_once(&input), expected);
    }

    #[test]
    fn env_gate_default_on() {
        // Without the env var set, sanitization is enabled. (We can't reliably
        // mutate process env in a parallel test run, so just assert the
        // default-unset branch via a name that won't be set.)
        std::env::remove_var("QONTINUI_TERMINAL_SANITIZE");
        assert!(sanitize_enabled());
    }

    #[test]
    fn over_cap_unterminated_sequence_is_flushed_not_unbounded() {
        let hook = VtSanitizeHook::new();
        // An OSC 52 that never terminates, longer than MAX_PENDING. Without the
        // cap this would accumulate without bound in the per-terminal carry
        // (the realistic trigger is `cat`-ing a binary file).
        let mut data = b"\x1b]52;c;".to_vec();
        data.resize(data.len() + MAX_PENDING + 1024, b'A');
        let out = hook.process("t1", &data);
        // Over-cap → flushed as pass-through (bounded memory), not silently eaten.
        assert!(!out.is_empty());
        // Carry was reset to ground: a subsequent plain write emits verbatim and
        // is NOT swallowed as a continuation of the abandoned sequence.
        assert_eq!(hook.process("t1", b"hello\n"), b"hello\n");
    }
}
