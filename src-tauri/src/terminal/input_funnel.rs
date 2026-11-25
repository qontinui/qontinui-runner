//! The pane's input funnel, exposed: who has written into this pane since it
//! was spawned, and whether its composer holds a draft.
//!
//! Plan `2026-10-28-one-session-lifecycle-authority`, Phase 80 (node R2-1).
//! [`super::session::TerminalSession::write`] has always been the single
//! funnel for raw terminal input, and `submit_prompt` / `insert_prompt` the
//! funnel for whole prompts, but what they recorded was only "the last write"
//! per choke point ([`super::session::PtyInputSlots`]), read by the
//! phantom-turn detector alone. Two things the lifecycle kernel reads were
//! missing:
//!
//! 1. **A runner-inserted draft was invisible.** `insert_prompt` (the review
//!    notes' "insert") pastes a prompt WITHOUT the submitting CR, so the pane's
//!    composer holds text nobody submitted. It used to be recorded in the
//!    `last_submit` slot, indistinguishable from a submitted turn, and it never
//!    reached `observe_input`. [`PaneDraftState`] now reads `Draft` for it.
//! 2. **Each write's writer was not kept.** D4's `closed_by_creator` arm needs
//!    "no one but the pane's creator has written into it since the spawn"
//!    (plan m6-3). [`InputFunnel`] keeps a bounded, writer-tagged history and,
//!    unbounded, the set of writer classes seen since the spawn. The HTTP
//!    create route's deferred initial command counts as its creator's write;
//!    the emulator's control-response chunks (xterm answering Claude Code's
//!    terminal queries, focus reports included) count as no one's input
//!    (plan NC7-3).
//!
//! Relayed input keeps the relay's provenance beside it
//! ([`RelayInputProvenance`]): the `origin` the relay stamps on a frame it
//! forwards from the operator's user-session path (Phase 16's web half), and a
//! source's `writer` stamp. Both are RECORDED, never authority: `origin`
//! counts only on a frame that carries no relay `remote` block (NC6-2), an
//! absent stamp proves nothing, and `writer` is diagnostic provenance only —
//! a device credential can set it (N5-2, NC5-3).
//!
//! This module is pure state and folds; it holds no lock and does no I/O.
//! The session keeps one [`InputFunnel`] beside its last-input slots and folds
//! every input event into it under the same lock.

use std::collections::{BTreeSet, VecDeque};
use std::time::Instant;

use super::session::PtyWriteCaller;

/// How many input records the per-pane history keeps. The since-spawn writer
/// set ([`InputFunnel::writers_since_spawn`]) is not bounded by this — it is
/// the part `closed_by_creator` reads, so it must survive a long session.
pub const INPUT_HISTORY_CAP: usize = 32;

/// The longest relay stamp kept, in bytes. The stamps arrive in a frame from
/// another process; a value is identity, not payload, and is cut rather than
/// stored whole.
pub const RELAY_STAMP_MAX_BYTES: usize = 64;

/// The relay's `origin` stamp for a frame forwarded from the operator's
/// authenticated user-session path (qontinui-web, Phase 16's web half).
pub const ORIGIN_USER_SESSION: &str = "user_session";

/// Where a relayed `terminal_input` frame came from, as the frame itself says.
///
/// Recorded beside [`PtyWriteCaller::RemoteTerminalInput`], never consulted
/// for authority here. [`Self::user_session_stamped`] is the one reading the
/// plan allows (D3, NC6-2), and even a `true` there is only as strong as the
/// relay that stamped it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct RelayInputProvenance {
    /// The frame carried a relay `remote` block — a REMOTE source's input
    /// admitted by the grant gate, not the relay's operator path.
    pub remote_block: bool,
    /// The relay's `origin` stamp, when the frame carried one.
    pub origin: Option<String>,
    /// A source's `writer` stamp, when the frame carried one. Diagnostic only.
    pub writer: Option<String>,
}

impl RelayInputProvenance {
    /// Read the stamps off a relayed `terminal_input` frame. `remote_block` is
    /// passed in rather than re-derived, so the caller records exactly the arm
    /// its grant gate took. A non-string or empty stamp reads as absent; a long
    /// one is cut to [`RELAY_STAMP_MAX_BYTES`].
    pub fn from_frame(data: &serde_json::Value, remote_block: bool) -> Self {
        Self {
            remote_block,
            origin: stamp(data, "origin"),
            writer: stamp(data, "writer"),
        }
    }

    /// Did the relay stamp this frame `origin: user_session` on a frame with no
    /// relay `remote` block? The only reading of `origin` the plan admits
    /// (NC6-2). `false` is not evidence of anything: an absent stamp proves
    /// nothing, and a relay predating Phase 16 stamps no frame.
    pub fn user_session_stamped(&self) -> bool {
        !self.remote_block && self.origin.as_deref() == Some(ORIGIN_USER_SESSION)
    }
}

fn stamp(data: &serde_json::Value, key: &str) -> Option<String> {
    let s = data.get(key)?.as_str()?;
    if s.is_empty() {
        return None;
    }
    Some(crate::str_utils::truncate_str(s, RELAY_STAMP_MAX_BYTES).to_string())
}

/// Which choke point an input event came through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputKind {
    /// Raw bytes through `write` (keystrokes, relayed input, launch lines).
    Write,
    /// A whole prompt pasted AND submitted (`submit_prompt`).
    Submit,
    /// A whole prompt pasted WITHOUT the submitting CR (`insert_prompt`): a
    /// runner-inserted draft.
    Insert,
    /// A `write` chunk made up solely of the emulator's own replies. Not input.
    ControlResponse,
}

/// Whose write an input event was, as far as THIS build can tell.
///
/// The classes `closed_by_creator` needs are [`Self::Creator`] and
/// [`Self::NoOne`]; every other class keeps a closed pane D4. Which SESSION
/// wrote an HTTP or proxy write is Phase 83's peer-pid lookup and is not
/// known here, so those read [`Self::Unresolved`] — never a guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InputWriter {
    /// The pane's own creation command — the deferred initial command line of
    /// the door that created it: the HTTP create route
    /// (`HttpCreateInitialCommand`, NC7-3) and the steward launch. Counts as
    /// the creator's write. `LaunchInitialCommand` maps here too, but has no
    /// producer today: the dashboard's "create with command" types its line
    /// through `terminal_write`, so it reads [`Self::PaneKeystroke`] — the
    /// conservative direction (such a pane never reads creator-only).
    Creator,
    /// Not input at all: the emulator answering a terminal query (NC7-3).
    NoOne,
    /// Input relayed from a remote end; its provenance rides the caller.
    Relayed,
    /// A runner producer delivering into the pane: a nudge, a coord message,
    /// an auto-response, an account migration, the wind-down `/exit`, a
    /// worker or transport sender. The wind-down's `/exit` is deliberately
    /// not the creator's: a pane the runner ended is not one its creator
    /// closed.
    Runner,
    /// The pane's own webview keystrokes (the Tauri `terminal_write` command).
    PaneKeystroke,
    /// A write through an HTTP, proxy or socket door whose writing session
    /// this build does not resolve (Phase 83 resolves it from the peer pid).
    Unresolved,
}

impl InputWriter {
    /// Classify one input event.
    pub fn of(caller: &PtyWriteCaller, kind: InputKind) -> Self {
        if kind == InputKind::ControlResponse {
            return Self::NoOne;
        }
        match caller {
            PtyWriteCaller::HttpCreateInitialCommand
            | PtyWriteCaller::LaunchInitialCommand
            | PtyWriteCaller::StewardLaunchCommand => Self::Creator,
            PtyWriteCaller::RemoteTerminalInput(_) => Self::Relayed,
            PtyWriteCaller::LoopingAgentNudge
            | PtyWriteCaller::AccountMigration
            | PtyWriteCaller::SessionMessagePoller
            | PtyWriteCaller::AutoResponse { .. }
            | PtyWriteCaller::WorkerSession
            | PtyWriteCaller::PtyTransport
            | PtyWriteCaller::ClaudeCliTransport
            | PtyWriteCaller::GracefulExit => Self::Runner,
            PtyWriteCaller::TauriTerminalWrite => Self::PaneKeystroke,
            PtyWriteCaller::HttpSubmitPrompt
            | PtyWriteCaller::ReviewNotes
            | PtyWriteCaller::TauriInvokeProxy
            | PtyWriteCaller::HttpWrite
            | PtyWriteCaller::WebSocketInput => Self::Unresolved,
            #[cfg(test)]
            PtyWriteCaller::Test => Self::Unresolved,
        }
    }
}

/// One input event in a pane's history. Never the bytes — keystrokes are
/// exactly what must not be kept — only who, when, how many, and which door.
#[derive(Clone, Debug)]
pub struct InputRecord {
    pub caller: PtyWriteCaller,
    pub writer: InputWriter,
    pub kind: InputKind,
    pub at: Instant,
    pub bytes: usize,
}

/// Where a pane's draft came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftSource {
    /// A runner producer pasted it without submitting (`insert_prompt`).
    Inserted,
    /// Text written through `write` after the last submitting CR.
    Typed,
}

/// Why the funnel cannot say whether the composer holds a draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftUnknownReason {
    /// The funnel did not observe this pane from its spawn: an adopted
    /// PTY-holder pane or a remote mirror, whose earlier input this process
    /// never saw. Cleared by the next submit.
    NotObservedFromSpawn,
    /// The bytes after the last submit included editing or control keys
    /// (backspace, arrows, Ctrl-U, Escape, …) whose effect on the composer the
    /// funnel does not model. Cleared by the next submit.
    EditingInput,
}

/// The pane's composer, as the input funnel reads it — the kernel's I axis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneDraftState {
    /// No input since the last submit leaves text in the composer (or, on a
    /// pane observed from its spawn, no input at all).
    NoDraft,
    /// The composer holds text nobody submitted. `caller` and `at` are the
    /// event that BEGAN the draft: text typed onto it later keeps them (and
    /// its source), so `at` is the draft's start, not its last edit — the
    /// history carries the later writes.
    Draft {
        source: DraftSource,
        caller: PtyWriteCaller,
        at: Instant,
    },
    /// The funnel cannot tell; never read as `NoDraft`.
    Unknown { reason: DraftUnknownReason },
}

impl PaneDraftState {
    pub fn is_draft(&self) -> bool {
        matches!(self, Self::Draft { .. })
    }
}

/// The per-pane input funnel state: the draft, the recent writer-tagged
/// history, and the writer classes seen since the spawn.
///
/// A fresh value is NOT observed from the spawn: every read it answers is
/// UNKNOWN until [`Self::mark_observed_from_spawn`] says this process built
/// the pane and saw every byte written into it. Only the local spawn path
/// calls that, so an adopted holder pane or a remote mirror reads UNKNOWN by
/// construction rather than "nobody wrote".
#[derive(Clone, Debug)]
pub struct InputFunnel {
    observed_from_spawn: bool,
    draft: PaneDraftState,
    history: VecDeque<InputRecord>,
    dropped: u64,
    writers_since_spawn: BTreeSet<InputWriter>,
    /// What the draft fold carries from one `write` chunk to the next: a
    /// bracketed paste or a newline key split across two writes (a relayed
    /// or HTTP/socket paste arrives one frame per `write`).
    scan: ScanCarry,
}

impl Default for InputFunnel {
    fn default() -> Self {
        Self {
            observed_from_spawn: false,
            draft: PaneDraftState::Unknown {
                reason: DraftUnknownReason::NotObservedFromSpawn,
            },
            history: VecDeque::new(),
            dropped: 0,
            writers_since_spawn: BTreeSet::new(),
            scan: ScanCarry::default(),
        }
    }
}

impl InputFunnel {
    /// This process spawned the pane and every input since passes through it.
    /// Called once, by the local spawn path, before any input can arrive; a
    /// call after input was recorded is ignored (the earlier bytes were seen,
    /// but the claim "from the spawn" would then rest on the caller's timing).
    pub fn mark_observed_from_spawn(&mut self) {
        if self.history.is_empty() && self.dropped == 0 {
            self.observed_from_spawn = true;
            self.draft = PaneDraftState::NoDraft;
        }
    }

    pub fn observed_from_spawn(&self) -> bool {
        self.observed_from_spawn
    }

    /// The composer's draft state (the kernel's I axis).
    pub fn draft_state(&self) -> &PaneDraftState {
        &self.draft
    }

    /// The most recent input records, oldest first, at most
    /// [`INPUT_HISTORY_CAP`].
    pub fn history(&self) -> impl Iterator<Item = &InputRecord> {
        self.history.iter()
    }

    /// Records pushed out of the bounded history.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Every writer class seen since the spawn, `None` when the funnel did not
    /// observe the pane from its spawn (UNKNOWN, never "nobody").
    pub fn writers_since_spawn(&self) -> Option<&BTreeSet<InputWriter>> {
        self.observed_from_spawn
            .then_some(&self.writers_since_spawn)
    }

    /// Has no one but the pane's creator written into it since the spawn?
    /// The writer half of D4's `closed_by_creator` (m6-3): `Some(true)` when
    /// every input since the spawn was the creator's or no one's,
    /// `Some(false)` when any other writer wrote, `None` when the funnel did
    /// not observe the pane from its spawn. Which session the creator WAS is
    /// Phase 83's; this answers only the writer condition.
    pub fn written_only_by_creator(&self) -> Option<bool> {
        let writers = self.writers_since_spawn()?;
        Some(
            writers
                .iter()
                .all(|w| matches!(w, InputWriter::Creator | InputWriter::NoOne)),
        )
    }

    /// Fold one input event. `data` is the bytes for a [`InputKind::Write`]
    /// (the draft fold reads them); the paste kinds do not need them.
    ///
    /// A `submit_prompt` is TWO events: its paste block folds as an
    /// [`InputKind::Insert`] (the text is in the composer, unsent) and its
    /// submitting CR, once that is on the wire, as an [`InputKind::Submit`] —
    /// so a CR that fails to write leaves the funnel reading `Draft`, which is
    /// what the composer then holds.
    pub fn record(
        &mut self,
        caller: PtyWriteCaller,
        kind: InputKind,
        at: Instant,
        bytes: usize,
        data: &[u8],
    ) {
        let writer = InputWriter::of(&caller, kind);
        self.draft = self.fold_draft(&caller, kind, at, data);
        if self.observed_from_spawn {
            self.writers_since_spawn.insert(writer);
        }
        if self.history.len() == INPUT_HISTORY_CAP {
            self.history.pop_front();
            self.dropped += 1;
        }
        self.history.push_back(InputRecord {
            caller,
            writer,
            kind,
            at,
            bytes,
        });
    }

    /// The draft fold: the composer state after one input event.
    fn fold_draft(
        &mut self,
        caller: &PtyWriteCaller,
        kind: InputKind,
        at: Instant,
        data: &[u8],
    ) -> PaneDraftState {
        let state = &self.draft;
        match kind {
            InputKind::ControlResponse => state.clone(),
            // A whole-prompt submit's CR submits the composer, a pending
            // draft included; nothing is left half-scanned.
            InputKind::Submit => {
                self.scan = ScanCarry::default();
                PaneDraftState::NoDraft
            }
            // A runner paste block (complete, never split) onto any composer:
            // there is now text nobody submitted, and a runner put some of it
            // there. An inserted draft keeps its own start; any other
            // composer state becomes an `Inserted` draft, so "a runner insert
            // is in the composer" is readable off the draft state alone.
            InputKind::Insert => {
                self.scan = ScanCarry::default();
                match state {
                    d @ PaneDraftState::Draft {
                        source: DraftSource::Inserted,
                        ..
                    } => d.clone(),
                    _ => PaneDraftState::Draft {
                        source: DraftSource::Inserted,
                        caller: caller.clone(),
                        at,
                    },
                }
            }
            InputKind::Write => {
                let scan = scan_write(&mut self.scan, data);
                let state = &self.draft;
                let base = if scan.submitted {
                    PaneDraftState::NoDraft
                } else {
                    state.clone()
                };
                match scan.tail {
                    Tail::Empty => base,
                    Tail::Text => match base {
                        // More text onto an existing draft keeps its source:
                        // an inserted draft the operator types onto is still
                        // one nobody submitted.
                        d @ PaneDraftState::Draft { .. } => d,
                        // Text typed onto a composer whose content is unknown
                        // leaves it non-empty; a draft is a draft.
                        _ => PaneDraftState::Draft {
                            source: DraftSource::Typed,
                            caller: caller.clone(),
                            at,
                        },
                    },
                    Tail::Editing => PaneDraftState::Unknown {
                        reason: DraftUnknownReason::EditingInput,
                    },
                }
            }
        }
    }
}

/// What [`scan_write`] carries from one `write` chunk to the next.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ScanCarry {
    /// Inside a bracketed paste whose end marker has not arrived yet.
    in_paste: bool,
    /// The last byte outside a paste, so a CR in the next chunk can be read
    /// against a `\` or ESC that ended this one.
    prev: Option<u8>,
    /// The last byte outside a paste was a SUBMITTING CR, so an LF right after
    /// it is the second half of that Enter (the create and steward routes
    /// write `cmd\r\n`), not a Ctrl-J. It spans a chunk boundary on purpose
    /// (`cmd\r` then `\n`), so a Ctrl-J typed as the very next key after an
    /// Enter is read the same way — accepted: it leaves only a newline in an
    /// otherwise empty composer.
    cr_submitted: bool,
    /// Trailing bytes of the previous chunk that are the start of a paste
    /// marker (`ESC [ 2 0 …`), held so a marker split across two writes is
    /// matched whole. At most `PASTE_BEGIN.len() - 1` bytes.
    pending: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
enum Tail {
    Empty,
    Text,
    Editing,
}

struct WriteScan {
    /// A submitting CR outside a bracketed paste: the composer was submitted.
    submitted: bool,
    /// What follows the last submit.
    tail: Tail,
}

const PASTE_BEGIN: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Walk one `write` chunk, continuing from `carry`: find the last submitting
/// CR and classify what follows it.
///
/// Only a bare CR outside a bracketed paste submits. Claude Code reads three
/// other "Enter"s as a NEWLINE in the composer, and the fold cannot see what
/// that leaves there, so each makes the tail editing input (UNKNOWN, never
/// `NoDraft`): an LF (Ctrl-J), a CR right after a `\` (backslash-Enter), and a
/// CR right after an ESC (Shift+Enter once `/terminal-setup` has run). A
/// newline inside a paste is pasted text, and an LF right after a submitting
/// CR is that Enter's second half (`\r\n`). Paste state, the previous byte and
/// a trailing partial paste marker carry across chunks, so a paste, a marker
/// or a newline key split over two writes reads as one. While a partial
/// marker is held, the chunk's tail reads as editing input (UNKNOWN) — its
/// meaning arrives with the next write.
fn scan_write(carry: &mut ScanCarry, chunk: &[u8]) -> WriteScan {
    let mut buf = std::mem::take(&mut carry.pending);
    buf.extend_from_slice(chunk);
    let held = held_marker_prefix(&buf);
    let end = buf.len() - held;
    carry.pending = buf[end..].to_vec();
    let data = &buf[..end];

    let mut submitted = false;
    let mut tail = Tail::Empty;
    let mut i = 0;
    while i < data.len() {
        let rest = &data[i..];
        if rest.starts_with(PASTE_BEGIN) {
            carry.in_paste = true;
            carry.prev = None;
            carry.cr_submitted = false;
            if tail == Tail::Empty {
                tail = Tail::Text;
            }
            i += PASTE_BEGIN.len();
            continue;
        }
        if rest.starts_with(PASTE_END) {
            carry.in_paste = false;
            carry.prev = None;
            carry.cr_submitted = false;
            // A paste just ended, so the composer holds its text — even when
            // the paste's body arrived in an earlier chunk.
            if tail == Tail::Empty {
                tail = Tail::Text;
            }
            i += PASTE_END.len();
            continue;
        }
        let b = data[i];
        if carry.in_paste {
            if b == 0x1b {
                tail = Tail::Editing;
            } else if tail == Tail::Empty {
                tail = Tail::Text;
            }
        } else if b == b'\n' && carry.cr_submitted {
            // The LF of a `\r\n` Enter: the CR already submitted.
            carry.cr_submitted = false;
            carry.prev = Some(b);
        } else {
            let newline_key = matches!(carry.prev, Some(b'\\') | Some(0x1b));
            let submits = b == b'\r' && !newline_key;
            if submits {
                submitted = true;
                tail = Tail::Empty;
            } else if b == b'\t' || (b >= 0x20 && b != 0x7f) {
                if tail == Tail::Empty {
                    tail = Tail::Text;
                }
            } else {
                // LF, a newline-key CR, and every other control byte.
                tail = Tail::Editing;
            }
            carry.cr_submitted = submits;
            carry.prev = Some(b);
        }
        i += 1;
    }
    if held > 0 {
        // A partial marker is held for the next write: until it arrives the
        // composer cannot be read.
        tail = Tail::Editing;
    }
    WriteScan { submitted, tail }
}

/// How many trailing bytes of `buf` are a proper prefix of a paste marker
/// (and so must wait for the next chunk to be matched whole). Only a suffix
/// starting with ESC qualifies; at most `PASTE_BEGIN.len() - 1` bytes.
fn held_marker_prefix(buf: &[u8]) -> usize {
    let max = PASTE_BEGIN.len().min(PASTE_END.len()) - 1;
    for n in (1..=max.min(buf.len())).rev() {
        let suffix = &buf[buf.len() - n..];
        if PASTE_BEGIN.starts_with(suffix) || PASTE_END.starts_with(suffix) {
            return n;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Instant {
        Instant::now()
    }

    fn spawned() -> InputFunnel {
        let mut f = InputFunnel::default();
        f.mark_observed_from_spawn();
        f
    }

    fn write(f: &mut InputFunnel, caller: PtyWriteCaller, data: &[u8]) {
        f.record(caller, InputKind::Write, t(), data.len(), data);
    }

    // ---- the draft (the kernel's I axis) ---------------------------------

    /// The phase's gate: a runner-inserted draft reads `Draft`, attributed to
    /// its producer — the review notes' insert (`mcp/session_review.rs`).
    #[test]
    fn a_runner_inserted_draft_reads_draft() {
        let mut f = spawned();
        assert_eq!(f.draft_state(), &PaneDraftState::NoDraft);
        f.record(PtyWriteCaller::ReviewNotes, InputKind::Insert, t(), 12, b"");
        match f.draft_state() {
            PaneDraftState::Draft { source, caller, .. } => {
                assert_eq!(*source, DraftSource::Inserted);
                assert_eq!(*caller, PtyWriteCaller::ReviewNotes);
            }
            other => panic!("an inserted draft must read Draft, got {other:?}"),
        }
    }

    #[test]
    fn a_submit_clears_an_inserted_draft() {
        let mut f = spawned();
        f.record(PtyWriteCaller::ReviewNotes, InputKind::Insert, t(), 12, b"");
        f.record(
            PtyWriteCaller::WorkerSession,
            InputKind::Submit,
            t(),
            8,
            b"",
        );
        assert_eq!(f.draft_state(), &PaneDraftState::NoDraft);
    }

    #[test]
    fn a_typed_cr_submits_an_inserted_draft() {
        let mut f = spawned();
        f.record(PtyWriteCaller::ReviewNotes, InputKind::Insert, t(), 12, b"");
        write(&mut f, PtyWriteCaller::TauriTerminalWrite, b"\r");
        assert_eq!(f.draft_state(), &PaneDraftState::NoDraft);
    }

    #[test]
    fn typing_onto_an_inserted_draft_keeps_its_source() {
        let mut f = spawned();
        f.record(PtyWriteCaller::ReviewNotes, InputKind::Insert, t(), 12, b"");
        write(&mut f, PtyWriteCaller::TauriTerminalWrite, b"more");
        assert!(matches!(
            f.draft_state(),
            PaneDraftState::Draft {
                source: DraftSource::Inserted,
                ..
            }
        ));
    }

    #[test]
    fn typed_text_without_a_submit_is_a_draft() {
        let mut f = spawned();
        write(&mut f, PtyWriteCaller::TauriTerminalWrite, b"hello");
        assert!(matches!(
            f.draft_state(),
            PaneDraftState::Draft {
                source: DraftSource::Typed,
                ..
            }
        ));
        write(&mut f, PtyWriteCaller::TauriTerminalWrite, b" world\r");
        assert_eq!(f.draft_state(), &PaneDraftState::NoDraft);
    }

    #[test]
    fn text_after_the_last_submit_in_one_chunk_is_a_draft() {
        let mut f = spawned();
        write(&mut f, PtyWriteCaller::HttpWrite, b"one\rtwo");
        assert!(f.draft_state().is_draft());
    }

    #[test]
    fn a_newline_inside_a_bracketed_paste_does_not_submit() {
        let mut f = spawned();
        write(
            &mut f,
            PtyWriteCaller::TauriTerminalWrite,
            b"\x1b[200~a\nb\x1b[201~",
        );
        assert!(f.draft_state().is_draft(), "{:?}", f.draft_state());
        write(
            &mut f,
            PtyWriteCaller::TauriTerminalWrite,
            b"\x1b[200~a\nb\x1b[201~\r",
        );
        assert_eq!(f.draft_state(), &PaneDraftState::NoDraft);
    }

    #[test]
    fn editing_keys_make_the_draft_unknown_until_the_next_submit() {
        for keys in [&b"\x7f"[..], b"\x15", b"\x1b[D", b"ab\x08"] {
            let mut f = spawned();
            write(&mut f, PtyWriteCaller::TauriTerminalWrite, b"abc");
            write(&mut f, PtyWriteCaller::TauriTerminalWrite, keys);
            assert_eq!(
                f.draft_state(),
                &PaneDraftState::Unknown {
                    reason: DraftUnknownReason::EditingInput
                },
                "keys {keys:?}"
            );
            write(&mut f, PtyWriteCaller::TauriTerminalWrite, b"\r");
            assert_eq!(f.draft_state(), &PaneDraftState::NoDraft, "keys {keys:?}");
        }
    }

    /// Claude Code's newline keys add a line to the composer instead of
    /// submitting it: Ctrl-J (LF), backslash-Enter and Shift+Enter (ESC CR).
    /// The fold reads each as UNKNOWN — never `NoDraft` over a held draft —
    /// whether the key arrives in one write or split across two.
    #[test]
    fn newline_keys_are_not_a_submit() {
        let cases: [&[&[u8]]; 6] = [
            &[b"abc\n"],
            &[b"abc\\\r"],
            &[b"abc\x1b\r"],
            &[b"abc\\", b"\r"],
            &[b"abc\x1b", b"\r"],
            &[b"\n"],
        ];
        for chunks in cases {
            let mut f = spawned();
            for c in chunks {
                write(&mut f, PtyWriteCaller::TauriTerminalWrite, c);
            }
            assert_eq!(
                f.draft_state(),
                &PaneDraftState::Unknown {
                    reason: DraftUnknownReason::EditingInput
                },
                "chunks {chunks:?}"
            );
            write(&mut f, PtyWriteCaller::TauriTerminalWrite, b"\r");
            assert_eq!(
                f.draft_state(),
                &PaneDraftState::NoDraft,
                "chunks {chunks:?}"
            );
        }
    }

    /// A relayed or HTTP/socket paste arrives one frame per write: a newline
    /// in the second half of a split paste is pasted text, not a submit.
    #[test]
    fn a_paste_split_across_writes_does_not_submit() {
        let mut f = spawned();
        write(&mut f, PtyWriteCaller::HttpWrite, b"\x1b[200~line1");
        assert!(f.draft_state().is_draft());
        write(&mut f, PtyWriteCaller::HttpWrite, b"\nline2\n\x1b[201~");
        assert!(f.draft_state().is_draft(), "{:?}", f.draft_state());
        write(&mut f, PtyWriteCaller::HttpWrite, b"\r");
        assert_eq!(f.draft_state(), &PaneDraftState::NoDraft);
    }

    /// The HTTP create and steward routes write their command as `cmd\r\n`:
    /// the LF is that Enter's second half, so the pane reads `NoDraft`, not
    /// UNKNOWN — whether the LF arrives in the same write or the next one.
    #[test]
    fn a_crlf_enter_submits() {
        let cases: [&[&[u8]]; 2] = [&[b"claude\r\n"], &[b"claude\r", b"\n"]];
        for chunks in cases {
            let mut f = spawned();
            for c in chunks {
                write(&mut f, PtyWriteCaller::HttpCreateInitialCommand, c);
            }
            assert_eq!(
                f.draft_state(),
                &PaneDraftState::NoDraft,
                "chunks {chunks:?}"
            );
        }
        // A second LF is a Ctrl-J again.
        let mut f = spawned();
        write(&mut f, PtyWriteCaller::HttpWrite, b"x\r\n\n");
        assert!(matches!(f.draft_state(), PaneDraftState::Unknown { .. }));
    }

    /// A paste marker split across two writes is matched whole: the pane is
    /// not left "inside a paste", and a later bare CR still submits.
    #[test]
    fn a_paste_end_marker_split_across_writes_is_matched() {
        let mut f = spawned();
        write(&mut f, PtyWriteCaller::HttpWrite, b"\x1b[200~text\x1b[20");
        assert!(
            matches!(f.draft_state(), PaneDraftState::Unknown { .. }),
            "a held partial marker reads UNKNOWN until it completes"
        );
        write(&mut f, PtyWriteCaller::HttpWrite, b"1~");
        assert!(f.draft_state().is_draft(), "{:?}", f.draft_state());
        write(&mut f, PtyWriteCaller::HttpWrite, b"\r");
        assert_eq!(f.draft_state(), &PaneDraftState::NoDraft);

        // A split begin marker too.
        let mut f = spawned();
        write(&mut f, PtyWriteCaller::HttpWrite, b"\x1b[2");
        write(&mut f, PtyWriteCaller::HttpWrite, b"00~a\nb\x1b[201~");
        assert!(f.draft_state().is_draft(), "{:?}", f.draft_state());
    }

    /// A lone Escape and a later Enter cannot be told from Shift+Enter (ESC
    /// CR), so the fold reads the first Enter as a newline key — UNKNOWN —
    /// and needs a second Enter to read `NoDraft`. The safe direction.
    #[test]
    fn escape_then_enter_reads_unknown_until_a_second_enter() {
        let mut f = spawned();
        write(&mut f, PtyWriteCaller::TauriTerminalWrite, b"abc");
        write(&mut f, PtyWriteCaller::TauriTerminalWrite, b"\x1b");
        assert!(matches!(f.draft_state(), PaneDraftState::Unknown { .. }));
        write(&mut f, PtyWriteCaller::TauriTerminalWrite, b"\r");
        assert!(matches!(f.draft_state(), PaneDraftState::Unknown { .. }));
        write(&mut f, PtyWriteCaller::TauriTerminalWrite, b"\r");
        assert_eq!(f.draft_state(), &PaneDraftState::NoDraft);
    }

    /// A runner insert onto text the operator typed makes the draft
    /// `Inserted`: "a runner insert is in the composer" reads off the state.
    #[test]
    fn an_insert_onto_a_typed_draft_reads_inserted() {
        let mut f = spawned();
        write(&mut f, PtyWriteCaller::TauriTerminalWrite, b"typed");
        f.record(PtyWriteCaller::ReviewNotes, InputKind::Insert, t(), 9, b"");
        assert!(matches!(
            f.draft_state(),
            PaneDraftState::Draft {
                source: DraftSource::Inserted,
                caller: PtyWriteCaller::ReviewNotes,
                ..
            }
        ));
    }

    /// `submit_prompt` folds its paste as an insert and its CR, once written,
    /// as the submit: a CR that never reached the wire leaves `Draft`, which
    /// is what the composer then holds (`PROMPT_PARTIALLY_WRITTEN`).
    #[test]
    fn a_submit_whose_cr_never_landed_reads_draft() {
        let mut f = spawned();
        f.record(
            PtyWriteCaller::WorkerSession,
            InputKind::Insert,
            t(),
            9,
            b"",
        );
        assert!(f.draft_state().is_draft());
        f.record(
            PtyWriteCaller::WorkerSession,
            InputKind::Submit,
            t(),
            1,
            b"",
        );
        assert_eq!(f.draft_state(), &PaneDraftState::NoDraft);
    }

    #[test]
    fn a_control_response_changes_no_draft() {
        let mut f = spawned();
        f.record(
            PtyWriteCaller::TauriTerminalWrite,
            InputKind::ControlResponse,
            t(),
            3,
            b"\x1b[I",
        );
        assert_eq!(f.draft_state(), &PaneDraftState::NoDraft);
        f.record(PtyWriteCaller::ReviewNotes, InputKind::Insert, t(), 12, b"");
        f.record(
            PtyWriteCaller::TauriTerminalWrite,
            InputKind::ControlResponse,
            t(),
            3,
            b"\x1b[O",
        );
        assert!(f.draft_state().is_draft());
    }

    /// An adopted holder pane or a remote mirror: the funnel never saw its
    /// earlier input, so its draft is UNKNOWN — never `NoDraft` — until a
    /// submit settles it.
    #[test]
    fn a_pane_not_observed_from_its_spawn_reads_unknown() {
        let mut f = InputFunnel::default();
        assert_eq!(
            f.draft_state(),
            &PaneDraftState::Unknown {
                reason: DraftUnknownReason::NotObservedFromSpawn
            }
        );
        write(&mut f, PtyWriteCaller::TauriTerminalWrite, b"x");
        assert!(
            f.draft_state().is_draft(),
            "text typed is a draft whatever came before"
        );
        f.record(
            PtyWriteCaller::WorkerSession,
            InputKind::Submit,
            t(),
            4,
            b"",
        );
        assert_eq!(f.draft_state(), &PaneDraftState::NoDraft);
    }

    // ---- the writer history (closed_by_creator's writer condition) -------

    #[test]
    fn the_creators_initial_command_and_control_responses_keep_creator_only() {
        let mut f = spawned();
        assert_eq!(
            f.written_only_by_creator(),
            Some(true),
            "nothing written yet"
        );
        write(
            &mut f,
            PtyWriteCaller::HttpCreateInitialCommand,
            b"claude\r",
        );
        f.record(
            PtyWriteCaller::TauriTerminalWrite,
            InputKind::ControlResponse,
            t(),
            3,
            b"\x1b[I",
        );
        assert_eq!(f.written_only_by_creator(), Some(true));
        let writers: Vec<_> = f.writers_since_spawn().unwrap().iter().copied().collect();
        assert_eq!(writers, vec![InputWriter::Creator, InputWriter::NoOne]);
    }

    #[test]
    fn any_other_writer_breaks_creator_only() {
        let others = [
            (PtyWriteCaller::TauriTerminalWrite, InputKind::Write),
            (
                PtyWriteCaller::RemoteTerminalInput(RelayInputProvenance::default()),
                InputKind::Write,
            ),
            (PtyWriteCaller::SessionMessagePoller, InputKind::Submit),
            (PtyWriteCaller::HttpWrite, InputKind::Write),
            (PtyWriteCaller::ReviewNotes, InputKind::Insert),
            (PtyWriteCaller::GracefulExit, InputKind::Write),
        ];
        for (caller, kind) in others {
            let mut f = spawned();
            write(
                &mut f,
                PtyWriteCaller::HttpCreateInitialCommand,
                b"claude\r",
            );
            f.record(caller.clone(), kind, t(), 1, b"x");
            assert_eq!(
                f.written_only_by_creator(),
                Some(false),
                "{caller:?} {kind:?}"
            );
        }
    }

    #[test]
    fn a_pane_not_observed_from_its_spawn_has_unknown_writers() {
        let mut f = InputFunnel::default();
        write(
            &mut f,
            PtyWriteCaller::HttpCreateInitialCommand,
            b"claude\r",
        );
        assert_eq!(f.writers_since_spawn(), None);
        assert_eq!(f.written_only_by_creator(), None);
        // Marking after input was recorded is refused: "from the spawn" would
        // then rest on the caller's timing.
        f.mark_observed_from_spawn();
        assert!(!f.observed_from_spawn());
        assert_eq!(f.written_only_by_creator(), None);
    }

    #[test]
    fn the_history_is_bounded_but_the_writer_set_is_not() {
        let mut f = spawned();
        write(&mut f, PtyWriteCaller::TauriTerminalWrite, b"x");
        for _ in 0..(INPUT_HISTORY_CAP * 2) {
            write(&mut f, PtyWriteCaller::HttpCreateInitialCommand, b"y");
        }
        assert_eq!(f.history().count(), INPUT_HISTORY_CAP);
        assert_eq!(f.dropped(), (INPUT_HISTORY_CAP + 1) as u64);
        assert!(f.history().all(|r| r.writer == InputWriter::Creator));
        assert_eq!(
            f.written_only_by_creator(),
            Some(false),
            "an early keystroke pushed out of the ring still counts"
        );
    }

    #[test]
    fn every_caller_has_a_writer_class() {
        // The creation doors are the creator; the emulator's replies are no
        // one's, whatever door carried them.
        for c in [
            PtyWriteCaller::HttpCreateInitialCommand,
            PtyWriteCaller::LaunchInitialCommand,
            PtyWriteCaller::StewardLaunchCommand,
        ] {
            assert_eq!(InputWriter::of(&c, InputKind::Write), InputWriter::Creator);
            assert_eq!(
                InputWriter::of(&c, InputKind::ControlResponse),
                InputWriter::NoOne
            );
        }
        assert_eq!(
            InputWriter::of(
                &PtyWriteCaller::TauriTerminalWrite,
                InputKind::ControlResponse
            ),
            InputWriter::NoOne
        );
        assert_eq!(
            InputWriter::of(&PtyWriteCaller::HttpWrite, InputKind::Write),
            InputWriter::Unresolved
        );
    }

    // ---- relay provenance ------------------------------------------------

    #[test]
    fn relay_provenance_reads_the_stamps_and_never_trusts_a_remote_frame() {
        let frame = serde_json::json!({"origin": "user_session", "writer": "agent-7"});
        let p = RelayInputProvenance::from_frame(&frame, false);
        assert_eq!(p.origin.as_deref(), Some("user_session"));
        assert_eq!(p.writer.as_deref(), Some("agent-7"));
        assert!(p.user_session_stamped());

        // The same stamp on a frame with a relay `remote` block counts for
        // nothing (NC6-2).
        let remote = RelayInputProvenance::from_frame(&frame, true);
        assert!(!remote.user_session_stamped());

        // An absent, empty or non-string stamp is absent and proves nothing.
        for frame in [
            serde_json::json!({}),
            serde_json::json!({"origin": ""}),
            serde_json::json!({"origin": 7}),
            serde_json::json!({"origin": "device"}),
        ] {
            assert!(!RelayInputProvenance::from_frame(&frame, false).user_session_stamped());
        }
    }

    #[test]
    fn relay_stamps_are_cut_to_the_cap() {
        let long = "w".repeat(RELAY_STAMP_MAX_BYTES * 4);
        let frame = serde_json::json!({ "writer": long });
        let p = RelayInputProvenance::from_frame(&frame, true);
        assert!(p.writer.unwrap().len() <= RELAY_STAMP_MAX_BYTES);
    }
}
