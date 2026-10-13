//! The holder's terminal-MODE tracker: what a fresh consumer must be told
//! that the replayed ring tail no longer says.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! D14 / Phase 2b. After a runner restart both consumers of pane bytes (the
//! runner's `Grid` and the webview's xterm) are brand new, and each derives
//! its mode state ONLY from the bytes it is fed — a capped ring tail. `claude`
//! sets bracketed paste once, at start-up; an hour later that `?2004h` has left
//! the ring, and the rebuilt consumer believes paste is unbracketed. So the
//! holder keeps a mode tracker — NOT an emulator: no cell grid — over every
//! byte its PTY produces, and an attach that asks for it is bracketed by two
//! synthesized byte strings:
//!
//! - **Preamble** — sent before the tail; the state AT THE TAIL'S FIRST BYTE:
//!   the screen buffer (`?47` / `?1047` / `?1049`), then the window title as
//!   `OSC 2`. Screen selection must come first: a tail replayed onto the wrong
//!   buffer pollutes the primary screen. It is the state at the tail's START,
//!   not the newest state, because the tail itself may switch buffers — a tail
//!   that leaves the alternate screen must begin ON it.
//! - **Postamble** — inserted exactly at the attach-time end of the stream; the
//!   state THERE: input modes (`?1`, `?1000`–`?1006`, `?2004`), keypad mode
//!   (`DECKPAM` / `DECKPNM`), the kitty keyboard flag stack, and cursor
//!   visibility (`?25`). After the tail, not before (shpool's order), because a
//!   tail that begins mid-escape-sequence can garble mode state and the
//!   postamble is what corrects it.
//!
//! Rules (D14 and Risk 8):
//!
//! - **Only what was parsed, never a default.** A mode the pane never touched
//!   is never emitted; a mode it touched is emitted in the state it was LAST
//!   put in, set or reset. A tracker that sets mouse reporting the application
//!   never asked for is worse than none.
//! - **Last-change order.** Postamble DEC modes are emitted in the order of
//!   their last change. The mouse-protocol modes (`?1000`, `?1002`, `?1003`)
//!   are one setting in every consumer the runner feeds (xterm.js, xterm):
//!   setting one replaces the others and resetting ANY turns reporting off.
//!   Their final state therefore depends only on the final operation, and
//!   replaying each mode's last operation in that order reproduces it.
//! - **`?2026` is never synthesized** (zmx pitfall 1): synchronized output is a
//!   per-frame bracket, not a mode to restore. It is not in [`POSTAMBLE_DEC_MODES`].
//! - **cwd is metadata, not display bytes** (zmx pitfall 2). The last `OSC 7`
//!   path is kept here as raw bytes for the attach reply; it is never written
//!   into the stream.
//! - Tab stops are not restored (zmx pitfall 4).
//!
//! # Offsets
//!
//! The tracker is fed every byte the ring is, in order, so its byte count is
//! the ring's `end_offset`. Bytes are parsed ONE AT A TIME so every change is
//! stamped with the exact absolute offset at which it takes effect (one past
//! the byte that completed its sequence) — the preamble needs the state at an
//! arbitrary offset inside the ring, and a change attributed to the end of the
//! 16 KiB read that carried it would put a tail on the wrong buffer whenever a
//! switch fell inside the read that also holds the tail's first byte. Screen
//! and title changes are logged with their offsets back to the ring's start
//! ([`ModeTracker::forget_before`]); older ones are folded into a base state.
//!
//! DATA-PATH module: `source_guard` bans text decoding here. Title and cwd are
//! kept as BYTES; turning them into attach-reply strings is `metadata`'s job.

use std::collections::{BTreeMap, VecDeque};

/// DEC private modes the postamble restores, when — and only when — the pane
/// set or reset them. Never `?2026` (see the module docs), never a screen
/// buffer mode (that is the preamble's). `1` is DECCKM (application cursor
/// keys), `1000`–`1006` mouse reporting, focus events and mouse encodings,
/// `25` DECTCEM (cursor visibility), `2004` bracketed paste.
pub const POSTAMBLE_DEC_MODES: &[u16] = &[1, 1000, 1001, 1002, 1003, 1004, 1005, 1006, 25, 2004];

/// The alternate-screen modes. Any of them set selects the alternate buffer;
/// any of them reset returns to the primary one.
pub const SCREEN_MODES: &[u16] = &[47, 1047, 1049];

/// Kitty keyboard flag stack depth per screen (kitty's own bound): a push past
/// it drops the oldest entry.
pub const KITTY_STACK_MAX: usize = 8;

/// Longest window title kept, in bytes; a longer one is cut at this length.
/// `vte` already caps an OSC at 1 KiB, so this is a second fence, not the
/// first.
pub const MAX_TITLE_BYTES: usize = 1024;

/// Most screen/title changes logged between the ring's start and its end. Past
/// it the oldest are folded into the base early, which makes the preamble for a
/// tail starting before the folded changes APPROXIMATE (it reads the base,
/// which is newer than the tail's start). A pane switching buffers or titles
/// more than this often per ring of output is not a pane anyone resumes from
/// that far back; the bound keeps a hostile stream from growing the log.
pub const MAX_PREAMBLE_LOG: usize = 4096;

/// Which buffer the pane is drawing on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    #[default]
    Primary,
    /// The alternate buffer, entered with this DEC private mode (47, 1047 or
    /// 1049) — the preamble re-enters it the same way.
    Alternate(u16),
}

/// Everything the PREAMBLE says, at one stream position.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PreambleState {
    pub screen: Screen,
    /// The last `OSC 0` / `OSC 2` title, control bytes removed. `None` when the
    /// pane never set one.
    pub title: Option<Vec<u8>>,
}

/// One logged change to the preamble state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PreambleChange {
    Screen(Screen),
    Title(Vec<u8>),
}

impl PreambleState {
    fn apply(&mut self, change: &PreambleChange) {
        match change {
            PreambleChange::Screen(s) => self.screen = *s,
            PreambleChange::Title(t) => self.title = Some(t.clone()),
        }
    }

    /// The preamble bytes for this state: screen selection first, then the
    /// title. Empty when there is nothing to restore.
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Screen::Alternate(mode) = self.screen {
            out.extend_from_slice(format!("\x1b[?{mode}h").as_bytes());
        }
        if let Some(title) = &self.title {
            out.extend_from_slice(b"\x1b]2;");
            out.extend_from_slice(title);
            out.push(0x07);
        }
        out
    }
}

/// A kitty keyboard flag stack (one per screen, per the protocol). The first
/// entry is the base the stack never pops below; `=` modifies the top.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KittyStack {
    entries: Vec<u32>,
    /// The pane used the protocol on this screen at least once.
    touched: bool,
}

impl Default for KittyStack {
    fn default() -> Self {
        KittyStack {
            entries: vec![0],
            touched: false,
        }
    }
}

impl KittyStack {
    fn push(&mut self, flags: u32) {
        if self.entries.len() >= KITTY_STACK_MAX {
            self.entries.remove(0);
        }
        self.entries.push(flags);
        self.touched = true;
    }

    fn pop(&mut self, n: usize) {
        for _ in 0..n.max(1) {
            if self.entries.len() > 1 {
                self.entries.pop();
            } else {
                self.entries[0] = 0;
                break;
            }
        }
        self.touched = true;
    }

    fn set(&mut self, flags: u32, mode: u16) {
        if let Some(top) = self.entries.last_mut() {
            match mode {
                2 => *top |= flags,
                3 => *top &= !flags,
                _ => *top = flags,
            }
        }
        self.touched = true;
    }

    /// Bytes that put a consumer's stack into exactly this state, WHATEVER it
    /// held before (the tail may have pushed onto it): pop everything (which
    /// also zeroes the base), set the base, push the rest.
    fn restore_bytes(&self, out: &mut Vec<u8>) {
        if !self.touched {
            return;
        }
        out.extend_from_slice(format!("\x1b[<{}u", KITTY_STACK_MAX).as_bytes());
        let mut entries = self.entries.iter();
        if let Some(&base) = entries.next() {
            if base != 0 {
                out.extend_from_slice(format!("\x1b[={base};1u").as_bytes());
            }
        }
        for flags in entries {
            out.extend_from_slice(format!("\x1b[>{flags}u").as_bytes());
        }
    }
}

/// The newest mode state — what the POSTAMBLE says.
#[derive(Debug, Clone, Default)]
struct Modes {
    /// Postamble DEC modes the pane touched: (set?, change sequence number).
    dec: BTreeMap<u16, (bool, u64)>,
    /// Keypad mode the pane last selected: (application?, sequence number).
    keypad: Option<(bool, u64)>,
    /// Kitty stacks: [primary, alternate].
    kitty: [KittyStack; 2],
    preamble: PreambleState,
    /// The last `OSC 7` working directory, percent-decoded path bytes.
    cwd: Option<Vec<u8>>,
}

impl Modes {
    fn kitty_mut(&mut self) -> &mut KittyStack {
        let i = usize::from(self.preamble.screen != Screen::Primary);
        &mut self.kitty[i]
    }

    fn postamble(&self) -> Vec<u8> {
        let mut changes: Vec<(u64, Vec<u8>)> = self
            .dec
            .iter()
            .map(|(mode, (set, seq))| {
                let action = if *set { 'h' } else { 'l' };
                (*seq, format!("\x1b[?{mode}{action}").into_bytes())
            })
            .collect();
        if let Some((application, seq)) = self.keypad {
            let esc: &[u8] = if application { b"\x1b=" } else { b"\x1b>" };
            changes.push((seq, esc.to_vec()));
        }
        changes.sort_by_key(|(seq, _)| *seq);
        let mut out: Vec<u8> = changes.into_iter().flat_map(|(_, b)| b).collect();
        let i = usize::from(self.preamble.screen != Screen::Primary);
        self.kitty[i].restore_bytes(&mut out);
        out
    }
}

/// See the module docs.
pub struct ModeTracker {
    parser: vte::Parser,
    modes: Modes,
    /// Absolute offset of the next byte to be fed (== the ring's end offset).
    fed: u64,
    /// Monotonic change counter for last-change ordering.
    seq: u64,
    /// The preamble state at `base_at`.
    base: PreambleState,
    base_at: u64,
    /// Screen/title changes after `base_at`: (offset they take effect at, change).
    log: VecDeque<(u64, PreambleChange)>,
}

impl std::fmt::Debug for ModeTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModeTracker")
            .field("fed", &self.fed)
            .field("base_at", &self.base_at)
            .field("log_len", &self.log.len())
            .finish_non_exhaustive()
    }
}

impl Default for ModeTracker {
    fn default() -> Self {
        ModeTracker::new()
    }
}

/// What an attach that asked for a restore gets, computed under the pane lock
/// so it agrees with the ring it brackets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// The preamble, for the tail starting at `preamble_at`.
    pub preamble: Vec<u8>,
    pub preamble_at: u64,
    /// The postamble, to be inserted at stream offset `postamble_at`.
    pub postamble: Vec<u8>,
    pub postamble_at: u64,
}

impl ModeTracker {
    pub fn new() -> Self {
        ModeTracker {
            parser: vte::Parser::new(),
            modes: Modes::default(),
            fed: 0,
            seq: 0,
            base: PreambleState::default(),
            base_at: 0,
            log: VecDeque::new(),
        }
    }

    /// Parse `bytes`, which follow everything fed so far.
    pub fn feed(&mut self, bytes: &[u8]) {
        let ModeTracker {
            parser,
            modes,
            fed,
            seq,
            log,
            ..
        } = self;
        let mut perf = Recorder {
            modes,
            seq,
            log,
            at: 0,
        };
        for b in bytes {
            *fed += 1;
            perf.at = *fed;
            parser.advance(&mut perf, std::slice::from_ref(b));
        }
        while self.log.len() > MAX_PREAMBLE_LOG {
            self.fold_oldest();
        }
    }

    /// Absolute offset of the next byte to be fed.
    pub fn fed(&self) -> u64 {
        self.fed
    }

    fn fold_oldest(&mut self) {
        if let Some((at, change)) = self.log.pop_front() {
            self.base.apply(&change);
            self.base_at = at;
        }
    }

    /// Fold every change that takes effect at or before `offset` into the
    /// base. Called with the ring's start offset after each push: no tail can
    /// begin before it.
    pub fn forget_before(&mut self, offset: u64) {
        while self.log.front().is_some_and(|(at, _)| *at <= offset) {
            self.fold_oldest();
        }
        if offset > self.base_at {
            self.base_at = offset.min(self.fed);
        }
    }

    /// The preamble state for a tail whose first byte is at absolute `offset`:
    /// every change that took effect at or before it. An offset older than the
    /// base (see [`MAX_PREAMBLE_LOG`]) reads the base.
    pub fn preamble_state_at(&self, offset: u64) -> PreambleState {
        let mut st = self.base.clone();
        for (at, change) in &self.log {
            if *at > offset {
                break;
            }
            st.apply(change);
        }
        st
    }

    /// The postamble for the newest state (the stream position [`Self::fed`]).
    pub fn postamble(&self) -> Vec<u8> {
        self.modes.postamble()
    }

    /// The envelope for a tail that starts at `tail_at` and is followed, at the
    /// current end of the stream, by the postamble.
    pub fn envelope(&self, tail_at: u64) -> Envelope {
        Envelope {
            preamble: self.preamble_state_at(tail_at).bytes(),
            preamble_at: tail_at,
            postamble: self.postamble(),
            postamble_at: self.fed,
        }
    }

    /// The newest title (raw bytes, control bytes removed).
    pub fn title(&self) -> Option<&[u8]> {
        self.modes.preamble.title.as_deref()
    }

    /// The newest `OSC 7` working directory (percent-decoded path bytes).
    pub fn cwd(&self) -> Option<&[u8]> {
        self.modes.cwd.as_deref()
    }
}

/// The `vte` performer: records changes, stamped with `at`.
struct Recorder<'a> {
    modes: &'a mut Modes,
    seq: &'a mut u64,
    log: &'a mut VecDeque<(u64, PreambleChange)>,
    /// The offset the current byte's changes take effect at.
    at: u64,
}

impl Recorder<'_> {
    fn next_seq(&mut self) -> u64 {
        *self.seq += 1;
        *self.seq
    }

    fn change_preamble(&mut self, change: PreambleChange) {
        self.modes.preamble.apply(&change);
        self.log.push_back((self.at, change));
    }

    fn set_dec(&mut self, mode: u16, set: bool) {
        if SCREEN_MODES.contains(&mode) {
            let screen = if set {
                Screen::Alternate(mode)
            } else {
                Screen::Primary
            };
            if screen != self.modes.preamble.screen {
                self.change_preamble(PreambleChange::Screen(screen));
            }
        } else if POSTAMBLE_DEC_MODES.contains(&mode) {
            let seq = self.next_seq();
            self.modes.dec.insert(mode, (set, seq));
        }
    }

    /// `ESC c` (RIS): every mode back to its power-on default, which a fresh
    /// consumer already has — so nothing about them is left to restore. The
    /// title and cwd are not terminal modes and survive it.
    fn full_reset(&mut self) {
        self.modes.dec.clear();
        self.modes.keypad = None;
        self.modes.kitty = Default::default();
        if self.modes.preamble.screen != Screen::Primary {
            self.change_preamble(PreambleChange::Screen(Screen::Primary));
        }
    }

    /// `CSI ! p` (DECSTR): of the tracked modes, a soft reset sets cursor keys
    /// to normal, the keypad to numeric, and makes the cursor visible.
    fn soft_reset(&mut self) {
        self.set_dec(1, false);
        let seq = self.next_seq();
        self.modes.keypad = Some((false, seq));
        self.set_dec(25, true);
    }
}

/// Join an OSC's parameters after the first with the `;` that `vte` split on.
fn osc_payload(params: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, p) in params.iter().skip(1).enumerate() {
        if i > 0 {
            out.push(b';');
        }
        out.extend_from_slice(p);
    }
    out
}

/// `file://<host>/<path>` → the percent-decoded path bytes. `None` for any
/// other scheme or a malformed escape. On Windows `file://host/C:/x` names
/// `C:/x`, so the slash before a drive letter is dropped there.
pub fn osc7_path(uri: &[u8]) -> Option<Vec<u8>> {
    let rest = uri.strip_prefix(b"file://")?;
    let slash = rest.iter().position(|&b| b == b'/')?;
    let encoded = rest.get(slash..)?;
    let mut path = Vec::with_capacity(encoded.len());
    let mut i = 0;
    while let Some(&b) = encoded.get(i) {
        if b == b'%' {
            let hi = hex(*encoded.get(i + 1)?)?;
            let lo = hex(*encoded.get(i + 2)?)?;
            path.push(hi << 4 | lo);
            i += 3;
        } else {
            path.push(b);
            i += 1;
        }
    }
    if cfg!(windows)
        && path.len() >= 3
        && path[0] == b'/'
        && path[1].is_ascii_alphabetic()
        && path[2] == b':'
    {
        path.remove(0);
    }
    // A NUL cannot be in a path (zmx's OSC 7 formatter carried a stray one).
    if path.is_empty() || path.contains(&0) {
        return None;
    }
    Some(path)
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

impl vte::Perform for Recorder<'_> {
    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        if ignore {
            return;
        }
        let first = || {
            params
                .iter()
                .next()
                .and_then(|p| p.first().copied())
                .unwrap_or(0)
        };
        match (intermediates, action) {
            (b"?", 'h' | 'l') => {
                let set = action == 'h';
                for p in params.iter().flatten() {
                    self.set_dec(*p, set);
                }
            }
            // Kitty keyboard protocol: push / pop / set flags.
            (b">", 'u') => self.modes.kitty_mut().push(u32::from(first())),
            (b"<", 'u') => self.modes.kitty_mut().pop(usize::from(first())),
            (b"=", 'u') => {
                let mode = params
                    .iter()
                    .nth(1)
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1);
                self.modes.kitty_mut().set(u32::from(first()), mode);
            }
            (b"!", 'p') => self.soft_reset(),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        match byte {
            b'=' => {
                let seq = self.next_seq();
                self.modes.keypad = Some((true, seq));
            }
            b'>' => {
                let seq = self.next_seq();
                self.modes.keypad = Some((false, seq));
            }
            b'c' => self.full_reset(),
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        match params.first() {
            Some(&b"0") | Some(&b"2") if params.len() >= 2 => {
                let mut title: Vec<u8> = osc_payload(params)
                    .into_iter()
                    .filter(|&b| b >= 0x20 && b != 0x7f)
                    .collect();
                title.truncate(MAX_TITLE_BYTES);
                if self.modes.preamble.title.as_deref() != Some(&title[..]) {
                    self.change_preamble(PreambleChange::Title(title));
                }
            }
            Some(&b"7") if params.len() >= 2 => {
                if let Some(path) = osc7_path(&osc_payload(params)) {
                    self.modes.cwd = Some(path);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fed(bytes: &[u8]) -> ModeTracker {
        let mut t = ModeTracker::new();
        t.feed(bytes);
        t
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    /// Phase 2b test (3), holder half: the postamble NEVER carries `?2026`,
    /// whatever state the pane left synchronized output in — and neither does
    /// the preamble.
    #[test]
    fn pty_holder_attach_envelope_never_synthesizes_2026() {
        for stream in [
            &b"\x1b[?2026h"[..],
            b"\x1b[?2026l",
            b"\x1b[?2004;2026h\x1b[?1049h\x1b]2;t\x07\x1b[?2026h",
            b"\x1b[?2026;25;1h\x1b[?2026l\x1b[?2026h",
        ] {
            let t = fed(stream);
            let env = t.envelope(t.fed());
            assert!(!contains(&env.postamble, b"2026"), "{:?}", env.postamble);
            assert!(!contains(&env.preamble, b"2026"), "{:?}", env.preamble);
        }
    }

    /// Only what was parsed: a pane that touched nothing gets nothing.
    #[test]
    fn pty_holder_attach_untouched_modes_are_never_emitted() {
        let t = fed(b"plain output\r\n\x1b[1;31mred\x1b[0m\x1b[?7h");
        assert_eq!(
            t.envelope(0),
            Envelope {
                preamble: Vec::new(),
                preamble_at: 0,
                postamble: Vec::new(),
                postamble_at: t.fed(),
            }
        );
    }

    /// Each touched mode is emitted in its LAST state, in last-change order —
    /// which reproduces the mouse-protocol group's final state.
    #[test]
    fn pty_holder_attach_postamble_replays_last_changes_in_order() {
        let t = fed(b"\x1b[?1000h\x1b[?2004h\x1b[?1002h\x1b[?1002l\x1b[?25l\x1b=\x1b[?1h");
        assert_eq!(
            t.postamble(),
            b"\x1b[?1000h\x1b[?2004h\x1b[?1002l\x1b[?25l\x1b=\x1b[?1h".to_vec()
        );
        let t = fed(b"\x1b=\x1b[?1;2004h\x1b>\x1b[?2004l");
        assert_eq!(t.postamble(), b"\x1b[?1h\x1b>\x1b[?2004l".to_vec());
    }

    /// The preamble is the state at the TAIL's start: a tail that leaves the
    /// alternate screen must begin on it; one that enters it begins on the
    /// primary. Offsets are exact to the byte.
    #[test]
    fn pty_holder_attach_preamble_is_the_state_at_the_tail_start() {
        let enter = b"\x1b[?1049h";
        let mut t = ModeTracker::new();
        t.feed(b"aaaa");
        t.feed(enter);
        let entered_at = t.fed();
        t.feed(b"\x1b]0;one\x07bbbb\x1b[?1049l\x1b]2;two\x07cc");
        assert_eq!(t.preamble_state_at(4).bytes(), Vec::<u8>::new());
        // The last byte of `?1049h` is offset entered_at - 1: a tail starting
        // there begins INSIDE the sequence and on the primary screen.
        assert_eq!(t.preamble_state_at(entered_at - 1).screen, Screen::Primary);
        assert_eq!(
            t.preamble_state_at(entered_at).screen,
            Screen::Alternate(1049)
        );
        let mid = entered_at + b"\x1b]0;one\x07bb".len() as u64;
        assert_eq!(
            t.preamble_state_at(mid).bytes(),
            b"\x1b[?1049h\x1b]2;one\x07".to_vec()
        );
        assert_eq!(
            t.preamble_state_at(t.fed()).bytes(),
            b"\x1b]2;two\x07".to_vec()
        );
        assert_eq!(t.title(), Some(&b"two"[..]));
    }

    /// Folding the log into the base at the ring's start keeps the answer for
    /// every offset still in the ring.
    #[test]
    fn pty_holder_attach_folding_keeps_the_answer_inside_the_ring() {
        let mut t = ModeTracker::new();
        t.feed(b"\x1b[?47h\x1b]2;a\x07xx\x1b]2;b\x07yy");
        let all: Vec<PreambleState> = (0..=t.fed()).map(|o| t.preamble_state_at(o)).collect();
        let ring_start = 10;
        t.forget_before(ring_start);
        for o in ring_start..=t.fed() {
            assert_eq!(t.preamble_state_at(o), all[o as usize], "offset {o}");
        }
    }

    /// A kitty stack restore resets whatever the consumer holds, then rebuilds
    /// this one; stacks are per screen.
    #[test]
    fn pty_holder_attach_kitty_stack_restores_exactly() {
        let t = fed(b"\x1b[>1u\x1b[>3u\x1b[=5;2u");
        assert_eq!(t.postamble(), b"\x1b[<8u\x1b[>1u\x1b[>7u".to_vec());
        // On the alternate screen the primary's stack is not the one in force.
        let t = fed(b"\x1b[>1u\x1b[?1049h");
        assert_eq!(t.postamble(), Vec::<u8>::new());
        // Popped back to the base: still restored (the tail may have pushed).
        let t = fed(b"\x1b[>1u\x1b[<u");
        assert_eq!(t.postamble(), b"\x1b[<8u".to_vec());
        let t = fed(b"\x1b[=9u");
        assert_eq!(t.postamble(), b"\x1b[<8u\x1b[=9;1u".to_vec());
    }

    /// RIS forgets every mode (a fresh consumer has the defaults already) and
    /// returns to the primary screen; DECSTR resets what it resets.
    #[test]
    fn pty_holder_attach_resets_are_tracked() {
        let t = fed(b"\x1b[?2004;1000h\x1b[?1049h\x1b=\x1bc");
        assert_eq!(t.postamble(), Vec::<u8>::new());
        assert_eq!(t.preamble_state_at(t.fed()).screen, Screen::Primary);
        let t = fed(b"\x1b[?1h\x1b=\x1b[?25l\x1b[!p");
        assert_eq!(t.postamble(), b"\x1b[?1l\x1b>\x1b[?25h".to_vec());
    }

    /// Titles: OSC 0 and 2, `;` kept, control bytes dropped; OSC 1 (icon) is
    /// not a title.
    #[test]
    fn pty_holder_attach_title_tracking() {
        let t = fed(b"\x1b]1;icon\x07");
        assert_eq!(t.title(), None);
        let t = fed(b"\x1b]2;a;b\x1b\\");
        assert_eq!(t.title(), Some(&b"a;b"[..]));
        let t = fed("\x1b]0;\u{2733} Claude\x07".as_bytes());
        assert_eq!(t.title(), Some("\u{2733} Claude".as_bytes()));
    }

    /// OSC 7 → percent-decoded path bytes; anything else is not a cwd.
    #[test]
    fn pty_holder_attach_osc7_cwd() {
        let t = fed(b"\x1b]7;file://host/tmp/a%20b;c\x07");
        assert_eq!(t.cwd(), Some(&b"/tmp/a b;c"[..]));
        assert_eq!(
            osc7_path(b"file:///x/%C3%A9"),
            Some("/x/\u{e9}".as_bytes().to_vec())
        );
        assert_eq!(osc7_path(b"http://h/x"), None);
        assert_eq!(osc7_path(b"file://host"), None);
        assert_eq!(osc7_path(b"file://h/x%2"), None);
        assert_eq!(osc7_path(b"file://h/x%00y"), None);
        // An OSC 7 the tracker cannot read leaves the last good one.
        let t = fed(b"\x1b]7;file:///good\x07\x1b]7;bad\x07");
        assert_eq!(t.cwd(), Some(&b"/good"[..]));
    }

    /// A sequence split across feeds parses as one.
    #[test]
    fn pty_holder_attach_sequences_split_across_reads() {
        let mut t = ModeTracker::new();
        for b in b"\x1b[?2004h" {
            t.feed(&[*b]);
        }
        assert_eq!(t.postamble(), b"\x1b[?2004h".to_vec());
    }

    /// The log is bounded.
    #[test]
    fn pty_holder_attach_preamble_log_is_bounded() {
        let mut t = ModeTracker::new();
        for i in 0..(MAX_PREAMBLE_LOG + 100) {
            t.feed(format!("\x1b]2;{i}\x07").as_bytes());
        }
        assert!(t.log.len() <= MAX_PREAMBLE_LOG);
        assert_eq!(
            t.preamble_state_at(t.fed()).title,
            Some(format!("{}", MAX_PREAMBLE_LOG + 99).into_bytes())
        );
    }
}
