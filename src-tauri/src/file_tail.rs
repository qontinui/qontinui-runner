//! Bounded reads of the END of a file — the runner's one tail reader.
//!
//! Two shapes over one bounded read:
//!
//! - [`read_tail_lossy`] — the last `max` bytes as one string. Transcript
//!   readers scan it backwards for the newest message.
//! - [`tail_lines`] — the last `n` lines, for a log view. It reads
//!   [`TAIL_FIRST_WINDOW`] from the end first, and widens ONCE, to the caller's
//!   `cap`, only when that window held fewer than `n` complete lines.
//!
//! Neither reads more than its bound, whatever the file's size, and that is the
//! point. The log reads [`tail_lines`] replaced each pulled a WHOLE file into
//! memory to return its last 100 lines. At UI start that file was the runner's
//! own day log — 2.9 GB measured — read on the main thread into a brk heap that
//! glibc never gave back (plan
//! `2026-11-11-runner-glibc-arena-retention-and-whole-log-tail`, Phase 2).
//!
//! Semantics both shapes share:
//!
//! - Decoding is LOSSY. An invalid UTF-8 byte damages the line it sits in,
//!   never the rest of the read. The line reader this replaced stopped at the
//!   first invalid line and returned the lines BEFORE it as the file's tail.
//! - A window that starts mid-file starts mid-line, so everything through its
//!   first `\n` is dropped. A window that happens to start exactly on a line
//!   boundary drops that first line too: telling the two apart would take the
//!   byte before the window, which is outside the bound.
//! - The read stops at the length the file had when it was opened, so a log
//!   appended to during the read cannot widen it.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// The first window [`tail_lines`] reads from the end of a file. It holds the
/// default 100-line view of an ordinary log, so most reads stop here.
pub const TAIL_FIRST_WINDOW: u64 = 64 * 1024;

/// The cap a log view passes to [`tail_lines`]: the most it reads of one file,
/// however large the file and however many lines were asked for.
pub const LOG_TAIL_CAP: u64 = 4 * 1024 * 1024;

/// The end of a file as lines — what [`tail_lines`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineTail {
    /// The last lines read, oldest first, at most the `n` asked for, each
    /// without its `\n` or `\r\n`. An unterminated final line is a line.
    ///
    /// When the newest line alone is longer than the bounded read, this holds
    /// that line's TAIL — the bytes of it the read reached — rather than
    /// nothing, which would read as an empty log.
    pub lines: Vec<String>,
    /// How many lines the file holds — `Some` ONLY when the read covered the
    /// whole file. A bounded tail of a larger file never counts it, because
    /// counting means reading every byte. `None` is "not counted", never zero.
    pub total_lines: Option<u64>,
    /// The file's length in bytes when it was opened.
    pub file_bytes: u64,
}

impl LineTail {
    /// Whether the read started at the file's first byte, so `total_lines` is
    /// known and `lines` holds every line there is, up to the `n` asked for.
    pub fn covered_whole_file(&self) -> bool {
        self.total_lines.is_some()
    }

    /// The tail held in `window`: the bytes from offset `start` to the end of
    /// a source `file_bytes` long.
    fn of_window(window: &[u8], start: u64, n: usize, file_bytes: u64) -> Self {
        if start == 0 {
            return Self {
                lines: last_lines(window, n),
                total_lines: Some(count_lines(window) as u64),
                file_bytes,
            };
        }
        let lines = match after_first_newline(window) {
            Some(body) if !body.is_empty() => last_lines(body, n),
            // No line ends before the window does: the newest line alone is
            // longer than the window, so its tail stands in for it.
            _ if n > 0 && !window.is_empty() => vec![line_tail(window)],
            _ => Vec::new(),
        };
        Self {
            lines,
            total_lines: None,
            file_bytes,
        }
    }
}

/// The last `max` bytes of `path`, decoded LOSSILY: an invalid UTF-8 byte
/// damages the line it sits in, never the whole read. When the read starts
/// mid-file its first (partial) line is dropped. `None` only on an I/O error.
///
/// At most `max` bytes are read: the length is taken when the file is opened,
/// so a file appended to meanwhile is read up to that length, not its new end.
pub fn read_tail_lossy(path: &Path, max: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    read_tail_lossy_from(&mut file, len, max).ok()
}

/// [`read_tail_lossy`] over any seekable source `len` bytes long.
fn read_tail_lossy_from<R: Read + Seek>(src: &mut R, len: u64, max: u64) -> io::Result<String> {
    let start = len.saturating_sub(max);
    let window = read_range(src, start, len - start)?;
    let text = if start == 0 {
        &window[..]
    } else {
        after_first_newline(&window).unwrap_or_default()
    };
    Ok(String::from_utf8_lossy(text).into_owned())
}

/// The last `n` lines of `path`, reading at most `cap` bytes of it. See
/// [`tail_lines_from`] for exactly what is read and what comes back.
pub fn tail_lines(path: &Path, n: usize, cap: u64) -> io::Result<LineTail> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    tail_lines_from(&mut file, len, n, cap)
}

/// [`tail_lines`] over any seekable source `len` bytes long.
///
/// Reads the last [`TAIL_FIRST_WINDOW`] bytes (`cap`, when that is smaller).
/// Only when that window held fewer than `n` complete lines and did not start
/// at the source's first byte does it widen, once, to the last `cap` bytes —
/// reading just the bytes in front of the first window and joining them on.
/// So it never reads more than `cap` bytes, and never seeks more than twice.
pub fn tail_lines_from<R: Read + Seek>(
    src: &mut R,
    len: u64,
    n: usize,
    cap: u64,
) -> io::Result<LineTail> {
    let first = len.min(cap).min(TAIL_FIRST_WINDOW);
    let start = len - first;
    let window = read_range(src, start, first)?;
    let widest = len.min(cap);
    // `first < widest` implies `start > 0`: the first window is a strict tail.
    if first < widest && lines_after_partial(&window) < n {
        let wide_start = len - widest;
        if let Some(wide) = widen(src, wide_start, start, &window)? {
            return Ok(LineTail::of_window(&wide, wide_start, n, len));
        }
    }
    Ok(LineTail::of_window(&window, start, n, len))
}

/// Bytes `[start, start + count)` of `src` — fewer only when it ends first.
fn read_range<R: Read + Seek>(src: &mut R, start: u64, count: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    read_range_into(src, start, count, &mut bytes)?;
    Ok(bytes)
}

/// [`read_range`], appended to `buf`: the one bounded read every window uses.
fn read_range_into<R: Read + Seek>(
    src: &mut R,
    start: u64,
    count: u64,
    buf: &mut Vec<u8>,
) -> io::Result<()> {
    src.seek(SeekFrom::Start(start))?;
    src.by_ref().take(count).read_to_end(buf)?;
    Ok(())
}

/// Bytes `[wide_start, start)` of `src` with `window` (the bytes from `start`
/// on) joined after them, in one buffer sized for both up front, so the join
/// never reallocates. `None` when the front read came up short: the source
/// shrank (rotated or truncated) between the two reads, and joining it on
/// would splice two different files.
fn widen<R: Read + Seek>(
    src: &mut R,
    wide_start: u64,
    start: u64,
    window: &[u8],
) -> io::Result<Option<Vec<u8>>> {
    let front = start - wide_start;
    let joined = front + window.len() as u64;
    let mut wide = Vec::with_capacity(usize::try_from(joined).unwrap_or(0));
    read_range_into(src, wide_start, front, &mut wide)?;
    if wide.len() as u64 != front {
        return Ok(None);
    }
    wide.extend_from_slice(window);
    Ok(Some(wide))
}

/// `window` after its first `\n`: the whole lines a window that starts
/// mid-line holds. `None` when it holds no `\n` at all.
fn after_first_newline(window: &[u8]) -> Option<&[u8]> {
    let at = window.iter().position(|&b| b == b'\n')?;
    window.get(at + 1..)
}

/// How many complete lines a window that starts mid-line holds.
fn lines_after_partial(window: &[u8]) -> usize {
    after_first_newline(window).map_or(0, count_lines)
}

/// How many lines `text` holds. A final `\n` ends the last line rather than
/// starting another, an unterminated final line counts, and empty text holds
/// none — the same lines `BufRead::lines` would yield.
fn count_lines(text: &[u8]) -> usize {
    if text.is_empty() {
        return 0;
    }
    let text = text.strip_suffix(b"\n").unwrap_or(text);
    text.iter().filter(|&&b| b == b'\n').count() + 1
}

/// The last `n` of the lines [`count_lines`] counts in `text`, oldest first.
fn last_lines(text: &[u8], n: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let text = text.strip_suffix(b"\n").unwrap_or(text);
    let mut lines: Vec<String> = text
        .rsplit(|&b| b == b'\n')
        .take(n)
        .map(decode_line)
        .collect();
    lines.reverse();
    lines
}

/// One line, less the `\r` of a `\r\n` terminator, decoded lossily.
fn decode_line(line: &[u8]) -> String {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    String::from_utf8_lossy(line).into_owned()
}

/// The tail of a line longer than the `window` holding it: the window less its
/// terminator, from the first character boundary on. A window can open in the
/// middle of a multi-byte character, whose stray bytes would decode as U+FFFD.
fn line_tail(window: &[u8]) -> String {
    let line = window.strip_suffix(b"\n").unwrap_or(window);
    let stray = line
        .iter()
        .take(3)
        .take_while(|&&b| b & 0xC0 == 0x80)
        .count();
    decode_line(line.get(stray..).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const GIB: u64 = 1024 * 1024 * 1024;

    /// A log that exists only as arithmetic, so a test can read the end of a
    /// gigabyte with no gigabyte on disk or in memory. Every line is `width`
    /// bytes: `L`, the line's index zero-padded to fill it, then `\n`. It
    /// counts the bytes read out of it and the seeks made on it.
    struct CountingLog {
        len: u64,
        width: u64,
        pos: u64,
        bytes_read: u64,
        seeks: u32,
    }

    impl CountingLog {
        fn new(lines: u64, width: u64) -> Self {
            assert!(width >= 3, "a line needs its marker, a digit and a newline");
            Self {
                len: lines * width,
                width,
                pos: 0,
                bytes_read: 0,
                seeks: 0,
            }
        }

        fn byte_at(&self, offset: u64) -> u8 {
            let (k, col) = (offset / self.width, offset % self.width);
            if col == 0 {
                return b'L';
            }
            if col == self.width - 1 {
                return b'\n';
            }
            // `col` counts the digits from the left; `place` from the right.
            let place = self.width - 2 - col;
            let digit = if place >= 20 {
                0
            } else {
                (k / 10u64.pow(place as u32)) % 10
            };
            b'0' + digit as u8
        }

        /// Line `k` as [`tail_lines`] should return it: no `\n`.
        fn line(&self, k: u64) -> String {
            format!("L{k:0>pad$}", pad = (self.width - 2) as usize)
        }
    }

    impl Read for CountingLog {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let left = usize::try_from(self.len.saturating_sub(self.pos)).unwrap_or(usize::MAX);
            let n = buf.len().min(left);
            for (i, slot) in buf[..n].iter_mut().enumerate() {
                *slot = self.byte_at(self.pos + i as u64);
            }
            self.pos += n as u64;
            self.bytes_read += n as u64;
            Ok(n)
        }
    }

    impl Seek for CountingLog {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.seeks += 1;
            let target = match to {
                SeekFrom::Start(p) => Some(p),
                SeekFrom::End(d) => self.len.checked_add_signed(d),
                SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            };
            self.pos = target
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
            Ok(self.pos)
        }
    }

    fn tail_of(bytes: &[u8], n: usize, cap: u64) -> LineTail {
        tail_lines_from(&mut Cursor::new(bytes), bytes.len() as u64, n, cap).unwrap()
    }

    /// The property the plan's gate names: the end of a 1 GiB source, in at
    /// most `cap` bytes read — here only the first window, since 100 lines of
    /// 16 bytes fit in it.
    #[test]
    fn a_1_gib_source_yields_its_last_lines_from_at_most_cap_bytes() {
        let lines = GIB / 16;
        let mut log = CountingLog::new(lines, 16);
        assert_eq!(log.len, GIB);
        let tail = tail_lines_from(&mut log, GIB, 100, LOG_TAIL_CAP).unwrap();
        let want: Vec<String> = (lines - 100..lines).map(|k| log.line(k)).collect();
        assert_eq!(tail.lines, want);
        assert_eq!(tail.total_lines, None, "a bounded tail never counts");
        assert!(!tail.covered_whole_file());
        assert_eq!(tail.file_bytes, GIB);
        assert!(log.bytes_read <= LOG_TAIL_CAP, "read {}", log.bytes_read);
        assert_eq!(log.bytes_read, TAIL_FIRST_WINDOW, "it did not widen");
        assert_eq!(log.seeks, 1);
    }

    /// Lines so long that the first window holds fewer than `n`: it widens
    /// once, to the cap, reading only the bytes in front of the first window.
    #[test]
    fn too_few_lines_in_the_first_window_widens_once_to_the_cap() {
        let width = 30 * 1024;
        let lines = GIB / width;
        let mut log = CountingLog::new(lines, width);
        let len = log.len;
        let tail = tail_lines_from(&mut log, len, 3, LOG_TAIL_CAP).unwrap();
        let want: Vec<String> = (lines - 3..lines).map(|k| log.line(k)).collect();
        assert_eq!(tail.lines, want);
        assert_eq!(tail.total_lines, None);
        assert_eq!(log.bytes_read, LOG_TAIL_CAP, "the cap, and not a byte more");
        assert_eq!(log.seeks, 2);
    }

    /// Asking for more lines than the cap can hold returns what it holds. The
    /// cap window here starts exactly on a line boundary, and that first line
    /// is dropped as partial all the same (see the module doc).
    #[test]
    fn more_lines_than_the_cap_holds_returns_what_it_holds() {
        let lines = GIB / 16;
        let mut log = CountingLog::new(lines, 16);
        let tail = tail_lines_from(&mut log, GIB, 1_000_000, LOG_TAIL_CAP).unwrap();
        let held = LOG_TAIL_CAP / 16 - 1;
        assert_eq!(tail.lines.len() as u64, held);
        assert_eq!(tail.lines.first(), Some(&log.line(lines - held)));
        assert_eq!(tail.lines.last(), Some(&log.line(lines - 1)));
        assert_eq!(log.bytes_read, LOG_TAIL_CAP);
    }

    /// A window that opens mid-line never returns that fragment as a line.
    #[test]
    fn a_window_opening_mid_line_drops_the_fragment() {
        let lines = 10_000;
        let mut log = CountingLog::new(lines, 17);
        let len = log.len;
        // 100 whole lines and the last 5 bytes of the line before them.
        let cap = 17 * 100 + 5;
        let tail = tail_lines_from(&mut log, len, 1_000, cap).unwrap();
        let want: Vec<String> = (lines - 100..lines).map(|k| log.line(k)).collect();
        assert_eq!(tail.lines, want);
        assert_eq!(log.bytes_read, cap);
    }

    #[test]
    fn a_file_smaller_than_the_window_is_read_whole_and_counted_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.log");
        let text: String = (0..250).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, &text).unwrap();
        let tail = tail_lines(&path, 100, LOG_TAIL_CAP).unwrap();
        let want: Vec<String> = (150..250).map(|i| format!("line {i}")).collect();
        assert_eq!(tail.lines, want);
        assert_eq!(tail.total_lines, Some(250));
        assert!(tail.covered_whole_file());
        assert_eq!(tail.file_bytes, text.len() as u64);
    }

    #[test]
    fn a_missing_file_is_an_error_not_an_empty_tail() {
        let dir = tempfile::tempdir().unwrap();
        let err = tail_lines(&dir.path().join("absent.log"), 10, LOG_TAIL_CAP).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn crlf_terminators_are_stripped() {
        let tail = tail_of(b"one\r\ntwo\r\n\r\nthree\r\n", 10, LOG_TAIL_CAP);
        assert_eq!(tail.lines, ["one", "two", "", "three"]);
        assert_eq!(tail.total_lines, Some(4));
    }

    #[test]
    fn a_final_line_without_a_newline_is_still_a_line() {
        let tail = tail_of(b"one\ntwo\nthree", 2, LOG_TAIL_CAP);
        assert_eq!(tail.lines, ["two", "three"]);
        assert_eq!(tail.total_lines, Some(3));
    }

    #[test]
    fn an_empty_file_has_no_lines_and_is_counted_as_zero() {
        let tail = tail_of(b"", 100, LOG_TAIL_CAP);
        assert!(tail.lines.is_empty());
        // Zero here is a COUNT, not a default: the read covered the file.
        assert_eq!(tail.total_lines, Some(0));
        assert_eq!(tail.file_bytes, 0);
    }

    /// An invalid UTF-8 sequence costs only the line it sits in. The reader
    /// this replaced (`BufRead::lines` + `map_while(Result::ok)`) stopped at
    /// that line and returned the lines before it as the file's tail.
    #[test]
    fn invalid_utf8_mid_file_does_not_truncate_the_tail() {
        let whole = tail_of(
            b"before\nbad \xff\xfe bytes\nafter 1\nafter 2\n",
            10,
            LOG_TAIL_CAP,
        );
        assert_eq!(
            whole.lines,
            ["before", "bad \u{FFFD}\u{FFFD} bytes", "after 1", "after 2"]
        );
        assert_eq!(whole.total_lines, Some(4));

        // The same inside a bounded window of a file larger than it.
        let mut bytes = b"filler\n".repeat(20_000);
        bytes.extend_from_slice(b"bad \xc3 byte\nafter\n");
        let tail = tail_of(&bytes, 3, LOG_TAIL_CAP);
        assert_eq!(tail.lines, ["filler", "bad \u{FFFD} byte", "after"]);
        assert_eq!(tail.total_lines, None);
    }

    /// A newest line longer than the cap: its tail — as much of it as the cap
    /// reaches — comes back as the one line, not an empty log.
    #[test]
    fn a_single_line_longer_than_the_cap_returns_its_tail() {
        let cap: usize = 1024;
        let mut bytes = b"an older line\n".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', 3 * cap));
        bytes.push(b'\n');
        let tail = tail_of(&bytes, 5, cap as u64);
        assert_eq!(tail.lines, ["x".repeat(cap - 1)]);
        assert_eq!(tail.total_lines, None);

        // Unterminated — a line still being written — reads the same way.
        bytes.pop();
        assert_eq!(tail_of(&bytes, 5, cap as u64).lines, ["x".repeat(cap)]);

        // A window opening mid-character starts at the next whole character:
        // 1023 bytes of two-byte `é`s begin with a stray continuation byte.
        let accents = "é".repeat(2 * cap).into_bytes();
        let tail = tail_of(&accents, 5, 1023);
        assert_eq!(tail.lines, ["é".repeat(511)]);
    }

    #[test]
    fn read_tail_lossy_reads_at_most_max_and_drops_the_partial_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        std::fs::write(&path, b"aaaa\nbbbb\ncc\xffcc\n").unwrap();
        // The last 8 bytes are "b\ncc\xffcc\n": the "b" fragment is dropped.
        assert_eq!(read_tail_lossy(&path, 8).as_deref(), Some("cc\u{FFFD}cc\n"));
        assert_eq!(
            read_tail_lossy(&path, 1024).as_deref(),
            Some("aaaa\nbbbb\ncc\u{FFFD}cc\n")
        );
        assert_eq!(read_tail_lossy(&dir.path().join("absent"), 8), None);
    }

    /// The length is fixed when the source is opened: bytes appended after
    /// that are never read, by the first window or by the widened read.
    #[test]
    fn bytes_appended_after_open_are_never_read() {
        let mut bytes = b"one\ntwo\n".to_vec();
        let opened = bytes.len() as u64;
        bytes.extend_from_slice(b"appended\n");
        let tail = tail_lines_from(&mut Cursor::new(&bytes), opened, 10, LOG_TAIL_CAP).unwrap();
        assert_eq!(tail.lines, ["one", "two"]);

        // Widened: 10 lines of 10 000 bytes, a first window too small for 10.
        let mut log = CountingLog::new(1_000, 10_000);
        let opened = 100 * 10_000;
        let tail = tail_lines_from(&mut log, opened, 10, LOG_TAIL_CAP).unwrap();
        let want: Vec<String> = (90..100).map(|k| log.line(k)).collect();
        assert_eq!(tail.lines, want);
        assert_eq!(log.seeks, 2, "the reader must have widened");
        assert_eq!(
            tail.total_lines,
            Some(100),
            "the 900 appended lines are not counted"
        );
        assert_eq!(
            log.bytes_read, opened,
            "the opened length, and not a byte past it"
        );

        // `bytes` still carries the appended line; 8 is "one\ntwo\n".
        let lossy = read_tail_lossy_from(&mut Cursor::new(&bytes), 8, 1024).unwrap();
        assert_eq!(lossy, "one\ntwo\n");
    }

    /// A source that shrinks between the two reads (rotated or truncated) is
    /// never spliced: the first window's tail stands on its own.
    #[test]
    fn a_source_that_shrinks_between_reads_is_not_spliced() {
        struct Shrinking {
            inner: Cursor<Vec<u8>>,
            seeks: u32,
            shrink_to: usize,
        }
        impl Read for Shrinking {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.inner.read(buf)
            }
        }
        impl Seek for Shrinking {
            fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
                self.seeks += 1;
                if self.seeks == 2 {
                    self.inner.get_mut().truncate(self.shrink_to);
                }
                self.inner.seek(to)
            }
        }
        // 200 lines of 1 000 bytes: the 64 KiB first window holds ~65 lines,
        // fewer than the 100 asked for, so the reader widens.
        let line = |k: usize| format!("{k:0>999}\n");
        let bytes: Vec<u8> = (0..200).flat_map(|k| line(k).into_bytes()).collect();
        let len = bytes.len() as u64;
        let mut src = Shrinking {
            inner: Cursor::new(bytes.clone()),
            seeks: 0,
            shrink_to: 1_000,
        };
        let shrunk = tail_lines_from(&mut src, len, 100, LOG_TAIL_CAP).unwrap();
        assert_eq!(src.seeks, 2, "the reader must have tried to widen");
        let first_only =
            tail_lines_from(&mut Cursor::new(&bytes), len, 100, TAIL_FIRST_WINDOW).unwrap();
        assert_eq!(shrunk.lines, first_only.lines);
        assert!(shrunk.lines.len() < 100);
    }

    /// The widened buffer is sized for the join up front: joining the first
    /// window on never grows it past the bytes it holds. The widened range is
    /// one byte short of the cap on purpose: a buffer grown by doubling from
    /// empty lands on exactly 2^22, so a range of exactly `LOG_TAIL_CAP` could
    /// not tell a pre-sized buffer from an unsized one.
    #[test]
    fn the_widened_buffer_is_sized_for_the_join() {
        let mut log = CountingLog::new(1_000, 10_000);
        let len = log.len;
        let start = len - TAIL_FIRST_WINDOW;
        let window = read_range(&mut log, start, TAIL_FIRST_WINDOW).unwrap();
        let wide = widen(&mut log, len - LOG_TAIL_CAP + 1, start, &window)
            .unwrap()
            .expect("the source did not shrink");
        assert_eq!(wide.len() as u64, LOG_TAIL_CAP - 1);
        assert!(
            wide.capacity() <= wide.len(),
            "capacity {} for {} bytes",
            wide.capacity(),
            wide.len()
        );
    }

    /// `tail_lines_from` widens through `widen`, so a revert to an inline,
    /// unsized join fails here rather than passing silently with `widen`
    /// left as dead code. Searches the production half's code only.
    #[test]
    fn tail_lines_from_widens_through_widen() {
        let src = include_str!("file_tail.rs");
        let (prod, _) = src.split_once("#[cfg(test)]").expect("a test module");
        let (_, rest) = prod
            .split_once("pub fn tail_lines_from<")
            .expect("tail_lines_from");
        let (body, _) = rest.split_once("\n}\n").expect("fn closed at column 0");
        // `process_helpers::code_lexer`: comments are removed and literals
        // emptied, so only call syntax counts.
        use crate::process_helpers::code_lexer::{code_only_from, Lex};
        let mut lex = Lex::Code;
        let code: Vec<String> = body
            .lines()
            .map(|line| code_only_from(line, &mut lex))
            .collect();
        assert!(
            code.iter().any(|line| line.contains("widen(src")),
            "tail_lines_from must widen through widen()"
        );
    }
}
