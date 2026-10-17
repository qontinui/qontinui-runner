//! The pixel-rule parameters `marker-golden.json` fixes (`codec.pixel`), read
//! from the golden file itself rather than restated here.
//!
//! The golden file is the contract between Phase 1's encoder and this decoder.
//! It is embedded at compile time (`include_str!`), so the binary is
//! self-contained AND rebuilds whenever the contract changes; [`spec`] parses
//! its `codec` block once. Every parameter is required: a golden file without
//! them is a broken contract, never a reason to fall back to a guess.

use serde::Serialize;
use serde_json::Value;
use std::sync::OnceLock;

/// The golden file, embedded.
pub const GOLDEN_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/termhost-bench/marker-golden.json"
));

/// `codec.pixel`'s numbers (see `codec.pixel.rules` for what each governs).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct PixelParams {
    /// A channel <= this is 0 (rule 2).
    pub channel_low_max: u8,
    /// A channel >= this is 1 (rule 2).
    pub channel_high_min: u8,
    /// |span / w - k| <= this, or `irregular_cells` (rule 7).
    pub width_tolerance: f64,
    /// A gap >= this x w holds impure cells; a red-cyan gap must be < it (rules 4-7).
    pub boundary_gap_max: f64,
}

/// The codec as the golden file states it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CodecSpec {
    pub version: u64,
    pub pixel: PixelParams,
    /// The row-stage rejections the pixel rules add before the cell checks.
    pub row_reasons: Vec<String>,
    /// The cell checks, in order.
    pub decode_order: Vec<String>,
}

fn req<'a>(obj: &'a Value, path: &str) -> Result<&'a Value, String> {
    path.split('.')
        .try_fold(obj, |v, key| v.get(key))
        .ok_or_else(|| format!("marker-golden.json has no codec.{path}"))
}

fn req_u8(obj: &Value, path: &str) -> Result<u8, String> {
    req(obj, path)?
        .as_u64()
        .and_then(|n| u8::try_from(n).ok())
        .ok_or_else(|| format!("marker-golden.json codec.{path} is not an integer 0..=255"))
}

fn req_f64(obj: &Value, path: &str) -> Result<f64, String> {
    req(obj, path)?
        .as_f64()
        .ok_or_else(|| format!("marker-golden.json codec.{path} is not a number"))
}

fn req_strings(obj: &Value, path: &str) -> Result<Vec<String>, String> {
    req(obj, path)?
        .as_array()
        .ok_or_else(|| format!("marker-golden.json codec.{path} is not a list"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("marker-golden.json codec.{path} holds a non-string"))
        })
        .collect()
}

impl CodecSpec {
    /// Parse the golden file's `codec` block.
    pub fn from_golden(json: &str) -> Result<CodecSpec, String> {
        let root: Value =
            serde_json::from_str(json).map_err(|e| format!("marker-golden.json: {e}"))?;
        let codec = root
            .get("codec")
            .ok_or_else(|| "marker-golden.json has no `codec` block".to_string())?;
        let pixel = PixelParams {
            channel_low_max: req_u8(codec, "pixel.channel_low_max")?,
            channel_high_min: req_u8(codec, "pixel.channel_high_min")?,
            width_tolerance: req_f64(codec, "pixel.width_tolerance")?,
            boundary_gap_max: req_f64(codec, "pixel.boundary_gap_max")?,
        };
        if pixel.channel_low_max >= pixel.channel_high_min {
            return Err(format!(
                "marker-golden.json channel bands overlap: {pixel:?}"
            ));
        }
        let in_range = pixel.width_tolerance > 0.0
            && pixel.width_tolerance < 0.5
            && pixel.boundary_gap_max > 0.0
            && pixel.boundary_gap_max <= 1.0;
        if !in_range {
            return Err(format!(
                "marker-golden.json pixel tolerances are out of range: {pixel:?}"
            ));
        }
        Ok(CodecSpec {
            version: req(codec, "version")?
                .as_u64()
                .ok_or("marker-golden.json codec.version is not an integer")?,
            pixel,
            row_reasons: req_strings(codec, "pixel.row_reasons")?,
            decode_order: req_strings(codec, "decode_order")?,
        })
    }
}

/// The parameters of the embedded golden file.
pub fn spec() -> &'static CodecSpec {
    static SPEC: OnceLock<CodecSpec> = OnceLock::new();
    SPEC.get_or_init(|| {
        CodecSpec::from_golden(GOLDEN_JSON)
            .expect("the embedded marker-golden.json must parse (the golden tests fail first if it does not)")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_golden_file_parses() {
        let s = spec();
        assert!(s.version >= 2);
        assert!(s.pixel.channel_low_max < 128 && s.pixel.channel_high_min > 128);
        assert_eq!(s.row_reasons, vec!["irregular_cells", "wrong_length"]);
        assert_eq!(s.decode_order.len(), 7);
    }

    #[test]
    fn missing_or_malformed_parameters_are_errors_not_defaults() {
        let full = r#"{"codec": {"version": 2, "decode_order": ["wrong_length"], "pixel": {"channel_low_max": 63, "channel_high_min": 192,
                      "width_tolerance": 0.25, "boundary_gap_max": 0.5, "row_reasons": ["irregular_cells", "wrong_length"]}}}"#;
        assert!(CodecSpec::from_golden(full).is_ok());
        let missing = full.replace(r#""width_tolerance": 0.25, "#, "");
        assert!(CodecSpec::from_golden(&missing).is_err());
        let overlap = full.replace(r#""channel_low_max": 63"#, r#""channel_low_max": 200"#);
        assert!(CodecSpec::from_golden(&overlap).is_err());
        assert!(CodecSpec::from_golden(r#"{"codec": {"version": 2}}"#).is_err());
    }
}
