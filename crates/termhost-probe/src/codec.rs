//! The termhost-bench marker codec at CELL level — the Rust twin of
//! `scripts/termhost-bench/marker.mjs` (Phase 1), which owns the format.
//!
//! A marker is 20 solid-background terminal cells encoding (kind, session,
//! sequence). Colours are the 8 corners of the xterm 256-colour cube (every
//! channel 0 or 255); digits are differential (`value[i] = (value[i-1] + 1 +
//! digit[i]) mod 8`, `value[-1] = 0`), so adjacent cells always differ. Layout,
//! 20 base-7 digits: start `[3, 6]`, kind (1), session (3), seq (6), checksum
//! (5), end `[5, 1, 3]`.
//!
//! This module decodes palette VALUES (0..=7) and xterm INDICES; the raster
//! module turns pixels into values by `codec.pixel`. Both report the first
//! failing check in the order `marker-golden.json` fixes (`codec.decode_order`).

use serde::Serialize;

/// Cells in one marker.
pub const CELLS: usize = 20;
/// Digit base.
pub const BASE: u32 = 7;
/// Start sentinel digits (always red, cyan).
pub const START_DIGITS: [u8; 2] = [3, 6];
/// End sentinel digits.
pub const END_DIGITS: [u8; 3] = [5, 1, 3];
/// Session digits (0..=342).
pub const SESSION_DIGITS: usize = 3;
/// Sequence digits (the counter mod 117649).
pub const SEQ_DIGITS: usize = 6;
/// Checksum digits.
pub const CHECKSUM_DIGITS: usize = 5;
/// Sessions 0..SESSION_LIMIT.
pub const SESSION_LIMIT: u32 = 343;
/// A marker carries the sequence counter modulo this (3.27 h at 10 Hz).
pub const SEQ_MODULUS: u32 = 117_649;
/// The largest prime below 7^5.
pub const CHECKSUM_MODULUS: u32 = 16_787;
/// Checksum multiplier.
pub const CHECKSUM_MULTIPLIER: u32 = 31;

const KIND_AT: usize = START_DIGITS.len();
const SESSION_AT: usize = KIND_AT + 1;
const SEQ_AT: usize = SESSION_AT + SESSION_DIGITS;
const CHECKSUM_AT: usize = SEQ_AT + SEQ_DIGITS;
const END_AT: usize = CELLS - END_DIGITS.len();

/// One palette colour: `value = 4R + 2G + B` with each channel bit set when the
/// channel is 255; `index` is the xterm 256-colour index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaletteColor {
    pub value: u8,
    pub index: u8,
    pub rgb: [u8; 3],
    pub name: &'static str,
}

const fn palette_color(value: u8, name: &'static str) -> PaletteColor {
    let r = (value >> 2) & 1;
    let g = (value >> 1) & 1;
    let b = value & 1;
    PaletteColor {
        value,
        index: 16 + 180 * r + 30 * g + 5 * b,
        rgb: [255 * r, 255 * g, 255 * b],
        name,
    }
}

/// The 8 binary-channel cube colours, by value.
pub const PALETTE: [PaletteColor; 8] = [
    palette_color(0, "black"),
    palette_color(1, "blue"),
    palette_color(2, "green"),
    palette_color(3, "cyan"),
    palette_color(4, "red"),
    palette_color(5, "magenta"),
    palette_color(6, "yellow"),
    palette_color(7, "white"),
];

/// Marker kinds. Digits 3..=6 are reserved and rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerKind {
    Output,
    Echo,
    Calibration,
}

impl MarkerKind {
    /// The kind digit.
    pub fn digit(self) -> u8 {
        match self {
            MarkerKind::Output => 0,
            MarkerKind::Echo => 1,
            MarkerKind::Calibration => 2,
        }
    }

    /// The kind for a digit, or `None` for a reserved digit.
    pub fn from_digit(d: u8) -> Option<Self> {
        match d {
            0 => Some(MarkerKind::Output),
            1 => Some(MarkerKind::Echo),
            2 => Some(MarkerKind::Calibration),
            _ => None,
        }
    }

    /// The name `marker.mjs` uses (`MARKER_KINDS` keys).
    pub fn as_str(self) -> &'static str {
        match self {
            MarkerKind::Output => "output",
            MarkerKind::Echo => "echo",
            MarkerKind::Calibration => "calibration",
        }
    }
}

/// A decoded (or to-be-encoded) marker. `seq` is already reduced mod
/// [`SEQ_MODULUS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct Marker {
    pub kind: MarkerKind,
    pub session: u16,
    pub seq: u32,
}

impl Marker {
    /// A marker; panics on an out-of-range session or sequence (a caller bug).
    pub fn new(kind: MarkerKind, session: u16, seq: u32) -> Self {
        assert!(
            u32::from(session) < SESSION_LIMIT,
            "session {session} outside 0..{SESSION_LIMIT}"
        );
        assert!(seq < SEQ_MODULUS, "seq {seq} outside 0..{SEQ_MODULUS}");
        Marker { kind, session, seq }
    }
}

/// Reduce an unbounded sequence counter to what a marker carries.
pub fn marker_seq(counter: u64) -> u32 {
    (counter % u64::from(SEQ_MODULUS)) as u32
}

/// Why a candidate is not a marker: the row-stage rejection the pixel rules
/// add (`irregular_cells`), then the cell checks in decode order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    /// A segment is not a whole number of cells (`codec.pixel` rule 7).
    IrregularCells,
    WrongLength,
    OffPalette,
    AdjacentCellsEqual,
    BadStartSentinel,
    BadEndSentinel,
    ReservedKind,
    ChecksumMismatch,
}

impl Rejection {
    /// Every reason, in the order the checks run (`codec.decode_order`).
    pub const DECODE_ORDER: [Rejection; 7] = [
        Rejection::WrongLength,
        Rejection::OffPalette,
        Rejection::AdjacentCellsEqual,
        Rejection::BadStartSentinel,
        Rejection::BadEndSentinel,
        Rejection::ReservedKind,
        Rejection::ChecksumMismatch,
    ];

    /// The row-stage rejections (`codec.pixel.row_reasons`).
    pub const ROW_REASONS: [Rejection; 2] = [Rejection::IrregularCells, Rejection::WrongLength];

    /// The reason string `marker-golden.json` uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Rejection::IrregularCells => "irregular_cells",
            Rejection::WrongLength => "wrong_length",
            Rejection::OffPalette => "off_palette",
            Rejection::AdjacentCellsEqual => "adjacent_cells_equal",
            Rejection::BadStartSentinel => "bad_start_sentinel",
            Rejection::BadEndSentinel => "bad_end_sentinel",
            Rejection::ReservedKind => "reserved_kind",
            Rejection::ChecksumMismatch => "checksum_mismatch",
        }
    }
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

fn to_base7<const N: usize>(mut n: u32) -> [u8; N] {
    let mut out = [0u8; N];
    for slot in out.iter_mut().rev() {
        *slot = (n % BASE) as u8;
        n /= BASE;
    }
    debug_assert_eq!(n, 0, "value does not fit in {N} base-7 digits");
    out
}

fn from_base7(digits: &[u8]) -> u32 {
    digits
        .iter()
        .fold(0u32, |acc, &d| acc * BASE + u32::from(d))
}

/// The checksum over the content digits (kind, session, seq).
pub fn checksum(content: &[u8]) -> u32 {
    content.iter().fold(0u32, |h, &d| {
        (h * CHECKSUM_MULTIPLIER + u32::from(d) + 1) % CHECKSUM_MODULUS
    })
}

/// The 20 digits of a marker, sentinels included.
pub fn encode_digits(marker: Marker) -> [u8; CELLS] {
    encode_digits_raw(marker.kind.digit(), marker.session, marker.seq)
}

/// [`encode_digits`] with a raw kind digit — exists to build reserved-kind
/// test vectors the way `marker.mjs` builds its golden one.
pub fn encode_digits_raw(kind_digit: u8, session: u16, seq: u32) -> [u8; CELLS] {
    assert!(
        u32::from(kind_digit) < BASE,
        "kind digit {kind_digit} is not base 7"
    );
    let session_digits: [u8; SESSION_DIGITS] = to_base7(u32::from(session));
    let seq_digits: [u8; SEQ_DIGITS] = to_base7(seq);
    let mut content = [0u8; 1 + SESSION_DIGITS + SEQ_DIGITS];
    content[0] = kind_digit;
    content[1..=SESSION_DIGITS].copy_from_slice(&session_digits);
    content[1 + SESSION_DIGITS..].copy_from_slice(&seq_digits);
    let sum: [u8; CHECKSUM_DIGITS] = to_base7(checksum(&content));

    let mut digits = [0u8; CELLS];
    digits[..KIND_AT].copy_from_slice(&START_DIGITS);
    digits[KIND_AT..CHECKSUM_AT].copy_from_slice(&content);
    digits[CHECKSUM_AT..END_AT].copy_from_slice(&sum);
    digits[END_AT..].copy_from_slice(&END_DIGITS);
    digits
}

/// Digits -> palette values by the differential rule.
pub fn digits_to_values(digits: &[u8; CELLS]) -> [u8; CELLS] {
    let mut prev = 0u8;
    let mut out = [0u8; CELLS];
    for (slot, &d) in out.iter_mut().zip(digits.iter()) {
        assert!(u32::from(d) < BASE, "digit {d} is not base 7");
        prev = (prev + 1 + d) % 8;
        *slot = prev;
    }
    out
}

/// Palette values -> digits (the inverse rule). A 7 means two adjacent cells
/// share a colour.
pub fn values_to_digits(values: &[u8]) -> Vec<u8> {
    let mut prev = 0i32;
    values
        .iter()
        .map(|&v| {
            let d = (i32::from(v) - prev - 1).rem_euclid(8) as u8;
            prev = i32::from(v);
            d
        })
        .collect()
}

/// A marker's palette values.
pub fn encode_values(marker: Marker) -> [u8; CELLS] {
    digits_to_values(&encode_digits(marker))
}

/// A marker's xterm 256-colour indices.
pub fn encode_indices(marker: Marker) -> [u8; CELLS] {
    encode_values(marker).map(|v| PALETTE[usize::from(v)].index)
}

/// A marker's cell colours as RGB.
pub fn encode_rgb(marker: Marker) -> [[u8; 3]; CELLS] {
    encode_values(marker).map(|v| PALETTE[usize::from(v)].rgb)
}

/// The marker overlay bytes the generator writes (`marker.mjs` `markerOverlay`):
/// save cursor, move to the 0-based (row, col), paint the cells, reset SGR,
/// restore cursor.
pub fn overlay_bytes(marker: Marker, row: u16, col: u16) -> Vec<u8> {
    let mut s = format!("\x1b7\x1b[{};{}H", u32::from(row) + 1, u32::from(col) + 1);
    s.push_str(&cells_sgr(&encode_indices(marker)));
    s.push_str("\x1b[0m\x1b8");
    s.into_bytes()
}

/// One space per cell under a 256-colour background (`marker.mjs` `cellsSgr`).
pub fn cells_sgr(indices: &[u8]) -> String {
    indices.iter().map(|i| format!("\x1b[48;5;{i}m ")).collect()
}

/// Decode cells that are each a palette value or `None` (a cell whose colour
/// is not one of the 8 — off palette). Checks run in [`Rejection::DECODE_ORDER`].
pub fn decode_cells(cells: &[Option<u8>]) -> Result<Marker, Rejection> {
    if cells.len() != CELLS {
        return Err(Rejection::WrongLength);
    }
    let mut values = [0u8; CELLS];
    for (slot, cell) in values.iter_mut().zip(cells) {
        match cell {
            Some(v) if *v < 8 => *slot = *v,
            _ => return Err(Rejection::OffPalette),
        }
    }
    if values.windows(2).any(|w| w[0] == w[1]) {
        return Err(Rejection::AdjacentCellsEqual);
    }
    let digits = values_to_digits(&values);
    if digits[..KIND_AT] != START_DIGITS {
        return Err(Rejection::BadStartSentinel);
    }
    if digits[END_AT..] != END_DIGITS {
        return Err(Rejection::BadEndSentinel);
    }
    let Some(kind) = MarkerKind::from_digit(digits[KIND_AT]) else {
        return Err(Rejection::ReservedKind);
    };
    let session = from_base7(&digits[SESSION_AT..SEQ_AT]);
    let seq = from_base7(&digits[SEQ_AT..CHECKSUM_AT]);
    if from_base7(&digits[CHECKSUM_AT..END_AT]) != checksum(&digits[KIND_AT..CHECKSUM_AT]) {
        return Err(Rejection::ChecksumMismatch);
    }
    Ok(Marker {
        kind,
        // Three base-7 digits are at most 342 and six at most 117648, so both
        // conversions are lossless by construction.
        session: session as u16,
        seq,
    })
}

/// Decode palette values (0..=7; anything else is off palette).
pub fn decode_values(values: &[u8]) -> Result<Marker, Rejection> {
    let cells: Vec<Option<u8>> = values.iter().map(|&v| (v < 8).then_some(v)).collect();
    decode_cells(&cells)
}

/// The palette value of an xterm 256-colour index, if it is one of the 8.
pub fn value_of_index(index: u16) -> Option<u8> {
    PALETTE
        .iter()
        .find(|p| u16::from(p.index) == index)
        .map(|p| p.value)
}

/// Decode xterm 256-colour indices; an index outside the palette is off palette.
pub fn decode_indices(indices: &[u16]) -> Result<Marker, Rejection> {
    if indices.len() != CELLS {
        return Err(Rejection::WrongLength);
    }
    let cells: Vec<Option<u8>> = indices.iter().map(|&i| value_of_index(i)).collect();
    decode_cells(&cells)
}

/// The RGB an xterm-compatible host renders for a 256-colour index 16..=255
/// (`marker.mjs` `xtermRgb`); `None` for the theme-dependent 0..=15.
pub fn xterm_rgb(index: u8) -> Option<[u8; 3]> {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        16..=231 => {
            let n = usize::from(index - 16);
            Some([LEVELS[n / 36], LEVELS[(n / 6) % 6], LEVELS[n % 6]])
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            Some([v, v, v])
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_matches_the_js_definition() {
        let indices: Vec<u8> = PALETTE.iter().map(|p| p.index).collect();
        assert_eq!(indices, vec![16, 21, 46, 51, 196, 201, 226, 231]);
        for p in PALETTE {
            assert_eq!(xterm_rgb(p.index), Some(p.rgb), "{}", p.name);
        }
    }

    #[test]
    fn encode_decode_round_trips_every_session_and_a_seq_sweep() {
        for kind in [
            MarkerKind::Output,
            MarkerKind::Echo,
            MarkerKind::Calibration,
        ] {
            for session in 0..SESSION_LIMIT as u16 {
                let seq = (u32::from(session) * 343 + 17) % SEQ_MODULUS;
                let m = Marker::new(kind, session, seq);
                assert_eq!(decode_values(&encode_values(m)), Ok(m));
            }
        }
        for seq in (0..SEQ_MODULUS).step_by(97) {
            let m = Marker::new(MarkerKind::Output, 7, seq);
            assert_eq!(decode_values(&encode_values(m)), Ok(m));
        }
    }

    #[test]
    fn adjacent_cells_always_differ_and_start_is_red_cyan() {
        for seq in (0..SEQ_MODULUS).step_by(1013) {
            let v = encode_values(Marker::new(MarkerKind::Echo, 99, seq));
            assert!(v.windows(2).all(|w| w[0] != w[1]));
            assert_eq!(&v[..2], &[4, 3]);
        }
    }

    #[test]
    fn every_single_cell_error_is_detected() {
        // The JS suite asserts this exhaustively at value level; the Rust
        // decoder must agree, because the raster decoder relies on it.
        for (session, seq) in [(0u16, 0u32), (14, 1234), (342, 117_648), (48, 16_806)] {
            let base = encode_values(Marker::new(MarkerKind::Output, session, seq));
            for i in 0..CELLS {
                for v in 0..8u8 {
                    if v == base[i] {
                        continue;
                    }
                    let mut altered = base;
                    altered[i] = v;
                    assert!(
                        decode_values(&altered).is_err(),
                        "cell {i} -> {v} misdecoded for ({session}, {seq})"
                    );
                }
            }
        }
    }

    #[test]
    fn checks_run_in_decode_order() {
        // Length before palette, palette before adjacency, and so on.
        assert_eq!(decode_values(&[4; 19]), Err(Rejection::WrongLength));
        let mut v = encode_values(Marker::new(MarkerKind::Output, 1, 1));
        v[3] = 9;
        v[4] = v[5];
        assert_eq!(decode_values(&v), Err(Rejection::OffPalette));
    }

    #[test]
    fn reserved_kinds_are_rejected_even_with_a_valid_checksum() {
        for k in 3..7u8 {
            let digits = encode_digits_raw(k, 14, 1234);
            assert_eq!(
                decode_values(&digits_to_values(&digits)),
                Err(Rejection::ReservedKind)
            );
        }
    }

    #[test]
    fn overlay_bytes_match_the_generator_form() {
        let m = Marker::new(MarkerKind::Output, 0, 0);
        let bytes = overlay_bytes(m, 0, 0);
        let text = String::from_utf8(bytes).expect("ascii");
        assert!(text.starts_with("\x1b7\x1b[1;1H\x1b[48;5;196m \x1b[48;5;51m "));
        assert!(text.ends_with("\x1b[0m\x1b8"));
    }
}
