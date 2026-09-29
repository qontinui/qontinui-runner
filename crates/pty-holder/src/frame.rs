//! The frame layer: length-prefixed, kind-tagged, byte-exact.
//!
//! ```text
//! +----------------------+--------+----------------------------+
//! | len: u32, big-endian | kind:1 | payload: len - 1 bytes     |
//! +----------------------+--------+----------------------------+
//! ```
//!
//! `len` counts every byte AFTER the prefix — the kind tag plus the payload —
//! so it is never zero. A `len` of zero or above [`MAX_FRAME_LEN`] is a protocol
//! error, not a frame.
//!
//! Two kinds are defined:
//!
//! - [`KIND_CONTROL`] — the payload is one JSON object (see `protocol`).
//! - [`KIND_DATA`] — the payload is RAW BYTES, carried as-is. Never re-encoded
//!   into text of any form and never a JSON array of numbers (a JSON byte array
//!   costs ~4x on the hot path, plan Phase 1). Pane output is not UTF-8 in
//!   general — a multibyte character can straddle a read boundary, and a TUI may
//!   emit arbitrary bytes — so any text conversion on this path is a corruption.
//!
//! The frame layer does not interpret `kind`: it round-trips every tag value,
//! and the SERVER decides which tags it accepts (a positive allowlist,
//! plan D5). That keeps the framing frozen (D15) while the verb set grows.
//!
//! This module is a DATA-PATH module: `source_guard` fails the build if a
//! text-decoding call appears in it.

use std::io::{self, Read, Write};

/// A control frame: the payload is a single JSON object.
pub const KIND_CONTROL: u8 = 0x01;
/// A data frame: the payload is raw bytes, byte-exact.
pub const KIND_DATA: u8 = 0x02;

/// Length of the big-endian `u32` prefix.
pub const PREFIX_LEN: usize = 4;

/// Largest `len` accepted (kind tag + payload): 16 MiB. A bound on what one
/// read allocates, so a corrupt or hostile prefix cannot ask for 4 GiB.
pub const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;

/// Largest payload a single frame can carry.
pub const MAX_PAYLOAD_LEN: usize = MAX_FRAME_LEN as usize - 1;

/// One frame, as read off or written to the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The kind tag. Not validated here — see the module docs.
    pub kind: u8,
    /// The payload bytes, exactly as sent.
    pub payload: Vec<u8>,
}

impl Frame {
    /// A control frame around an already-serialized JSON payload.
    pub fn control(payload: Vec<u8>) -> Self {
        Frame {
            kind: KIND_CONTROL,
            payload,
        }
    }

    /// A data frame carrying raw bytes.
    pub fn data(payload: Vec<u8>) -> Self {
        Frame {
            kind: KIND_DATA,
            payload,
        }
    }
}

/// Encode one frame into its exact wire bytes.
///
/// Fails only when the payload is too large to frame.
pub fn encode_frame(kind: u8, payload: &[u8]) -> io::Result<Vec<u8>> {
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "frame payload of {} bytes exceeds the {} byte limit",
                payload.len(),
                MAX_PAYLOAD_LEN
            ),
        ));
    }
    // Cannot overflow: payload.len() <= MAX_PAYLOAD_LEN < u32::MAX.
    let len = (payload.len() + 1) as u32;
    let mut out = Vec::with_capacity(PREFIX_LEN + 1 + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    Ok(out)
}

/// Write one frame with a SINGLE `write_all` of the whole encoded buffer, then
/// flush.
///
/// One buffer rather than prefix-then-payload so that two writers sharing a
/// stream under a lock can never interleave a prefix with someone else's body.
pub fn write_frame<W: Write + ?Sized>(w: &mut W, kind: u8, payload: &[u8]) -> io::Result<()> {
    let bytes = encode_frame(kind, payload)?;
    w.write_all(&bytes)?;
    w.flush()
}

/// Read one frame.
///
/// - `Ok(None)` — a clean end of stream exactly at a frame boundary.
/// - `Err(UnexpectedEof)` — the stream ended inside a frame.
/// - `Err(InvalidData)` — a zero or over-limit length prefix.
///
/// Short reads are normal: `read_exact` loops until the frame is complete, so a
/// frame split across any number of reads arrives whole and byte-identical.
pub fn read_frame<R: Read + ?Sized>(r: &mut R) -> io::Result<Option<Frame>> {
    let mut prefix = [0u8; PREFIX_LEN];
    // The first byte is read by hand: EOF here is a clean close, EOF anywhere
    // after it is a truncated frame.
    loop {
        match r.read(&mut prefix[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    r.read_exact(&mut prefix[1..])?;
    let len = u32::from_be_bytes(prefix);
    if len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame length 0: a frame carries at least its kind tag",
        ));
    }
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {len} exceeds the {MAX_FRAME_LEN} byte limit"),
        ));
    }
    let mut kind = [0u8; 1];
    r.read_exact(&mut kind)?;
    let mut payload = vec![0u8; len as usize - 1];
    r.read_exact(&mut payload)?;
    Ok(Some(Frame {
        kind: kind[0],
        payload,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A reader that hands out at most `step` bytes per call, cycling through
    /// the given step sizes, and injects an `Interrupted` before every read —
    /// the worst-behaved stream the frame layer must still reassemble.
    struct Trickle {
        data: Vec<u8>,
        pos: usize,
        steps: Vec<usize>,
        call: usize,
        interrupt_next: bool,
    }

    impl Trickle {
        fn new(data: Vec<u8>, steps: &[usize]) -> Self {
            Trickle {
                data,
                pos: 0,
                steps: steps.to_vec(),
                call: 0,
                interrupt_next: true,
            }
        }
    }

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.interrupt_next {
                self.interrupt_next = false;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "injected"));
            }
            self.interrupt_next = true;
            let step = self.steps[self.call % self.steps.len()];
            self.call += 1;
            let n = step.min(buf.len()).min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    fn all_256() -> Vec<u8> {
        (0u8..=255).collect()
    }

    /// Byte fidelity gate (plan Phase 1, amended bullet): all 256 byte values,
    /// invalid UTF-8, and frames split across many short reads, byte-identical
    /// on the far side.
    #[test]
    fn pty_holder_frame_byte_fidelity_round_trip() {
        let mut payloads: Vec<Vec<u8>> = vec![
            all_256(),
            all_256().into_iter().rev().collect(),
            // Invalid UTF-8: lone continuation bytes, an overlong encoding, a
            // truncated 4-byte sequence, a surrogate half, and 0xFF / 0xFE.
            vec![
                0x80, 0xBF, 0xC0, 0xAF, 0xF0, 0x9F, 0x98, 0xED, 0xA0, 0x80, 0xFF, 0xFE,
            ],
            // A valid multibyte char cut in half, as a PTY read boundary would.
            "é🙂".as_bytes()[..3].to_vec(),
            // NULs and escape sequences.
            vec![
                0x00, 0x00, 0x1B, b'[', b'?', b'2', b'0', b'0', b'4', b'h', 0x00,
            ],
            Vec::new(),
        ];
        // A large payload so a single frame spans hundreds of reads.
        payloads.push(
            (0..70_000u32)
                .map(|i| (i.wrapping_mul(31) % 256) as u8)
                .collect(),
        );

        let mut wire = Vec::new();
        for (i, p) in payloads.iter().enumerate() {
            let kind = if i % 2 == 0 { KIND_DATA } else { KIND_CONTROL };
            write_frame(&mut wire, kind, p).unwrap();
        }
        // An unknown kind tag must round-trip too: the frame layer is kind-blind.
        write_frame(&mut wire, 0x7F, &all_256()).unwrap();

        for steps in [
            &[1usize][..],
            &[1, 2, 3][..],
            &[5, 1, 7, 2][..],
            &[4096][..],
        ] {
            let mut r = Trickle::new(wire.clone(), steps);
            for (i, p) in payloads.iter().enumerate() {
                let f = read_frame(&mut r).unwrap().expect("a frame");
                let kind = if i % 2 == 0 { KIND_DATA } else { KIND_CONTROL };
                assert_eq!(f.kind, kind);
                assert_eq!(&f.payload, p, "payload {i} changed with steps {steps:?}");
            }
            let f = read_frame(&mut r).unwrap().expect("the 0x7F frame");
            assert_eq!(f.kind, 0x7F);
            assert_eq!(f.payload, all_256());
            assert!(read_frame(&mut r).unwrap().is_none(), "clean EOF");
        }
    }

    #[test]
    fn pty_holder_frame_exact_layout() {
        let bytes = encode_frame(KIND_DATA, &[0x00, 0xFF]).unwrap();
        assert_eq!(bytes, vec![0, 0, 0, 3, KIND_DATA, 0x00, 0xFF]);
        let bytes = encode_frame(KIND_CONTROL, &[]).unwrap();
        assert_eq!(bytes, vec![0, 0, 0, 1, KIND_CONTROL]);
    }

    #[test]
    fn pty_holder_frame_truncation_is_an_error_not_eof() {
        let full = encode_frame(KIND_DATA, b"hello").unwrap();
        for cut in 1..full.len() {
            let err = read_frame(&mut Cursor::new(full[..cut].to_vec())).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "cut at {cut}");
        }
        assert!(read_frame(&mut Cursor::new(Vec::new())).unwrap().is_none());
    }

    #[test]
    fn pty_holder_frame_rejects_zero_and_oversized_lengths() {
        let err = read_frame(&mut Cursor::new(vec![0, 0, 0, 0, 1])).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let over = (MAX_FRAME_LEN + 1).to_be_bytes();
        let err = read_frame(&mut Cursor::new(over.to_vec())).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            encode_frame(KIND_DATA, &vec![0u8; MAX_PAYLOAD_LEN + 1])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
