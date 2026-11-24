//! Pane METADATA for an attach reply: the bytes-to-text step the data path is
//! not allowed to take.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! D14: an attach that asks for a restore is told the pane's working directory
//! and title as metadata (they feed the runner's session record), alongside
//! the synthesized display bytes. The mode tracker (`crate::modes`) keeps both
//! as raw bytes — it is a data-path module, and `source_guard` bans text
//! decoding there. The reply is JSON, so the strings are made here, outside the
//! data path and outside the guard: nothing in this module ever touches stream
//! bytes, only the two short values the tracker extracted.
//!
//! Strict, never lossy: bytes that are not UTF-8 become `None` ("unknown"),
//! never a string with replacement characters that names a directory that
//! does not exist.

use std::ffi::OsString;

use crate::protocol::RestoreMeta;

/// `bytes` as a string when they are UTF-8, else `None`.
pub fn utf8_or_none(bytes: &[u8]) -> Option<String> {
    String::from_utf8(bytes.to_vec()).ok()
}

/// The restore metadata: the last `OSC 7` cwd the pane reported, else the
/// directory its child was started in (`spawn_cwd`, from the spec — what the
/// holder knows for certain when the program never reports one); and the last
/// title it set.
pub fn restore_meta(
    osc7_cwd: Option<&[u8]>,
    spawn_cwd: Option<&OsString>,
    title: Option<&[u8]>,
) -> RestoreMeta {
    let cwd = match osc7_cwd.and_then(utf8_or_none) {
        Some(c) => Some(c),
        None => spawn_cwd.and_then(|c| c.clone().into_string().ok()),
    };
    RestoreMeta {
        cwd,
        title: title.and_then(utf8_or_none),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_holder_attach_metadata_is_strict_utf8_with_the_spawn_cwd_as_fallback() {
        let spawn = OsString::from("/srv/start");
        assert_eq!(
            restore_meta(Some(b"/tmp/x"), Some(&spawn), Some("\u{2733} t".as_bytes())),
            RestoreMeta {
                cwd: Some("/tmp/x".into()),
                title: Some("\u{2733} t".into()),
            }
        );
        assert_eq!(
            restore_meta(Some(b"/bad\xff"), Some(&spawn), Some(b"\xfe")),
            RestoreMeta {
                cwd: Some("/srv/start".into()),
                title: None,
            }
        );
        assert_eq!(
            restore_meta(None, None, None),
            RestoreMeta {
                cwd: None,
                title: None,
            }
        );
    }
}
