//! Recognising npm's placeholder `claude` stub.
//!
//! Shared by the shim binary (`qontinui-shim`'s real-tool resolver) and the
//! runner binary (`agent_runtime::resolve_claude_bin`), so the two cannot
//! disagree about what a stub looks like. The bash identity shim carries the
//! same check as a builtins-only function (`identity_shim.bash`).

use std::io::Read;
use std::path::Path;

/// The message npm's placeholder `bin/claude.exe` prints, matched in its first
/// [`PLACEHOLDER_PROBE_BYTES`].
const NPM_PLACEHOLDER_MARKER: &[u8] = b"native binary not installed";
const PLACEHOLDER_PROBE_BYTES: u64 = 256;

/// Is `path` npm's PLACEHOLDER stub rather than the real tool?
///
/// `@anthropic-ai/claude-code` ships `bin/claude.exe` as a ~500-byte shell
/// script that only prints "claude native binary not installed" and exits 1;
/// its postinstall then links the native binary over it. So a reinstall has a
/// second window, after npm re-links `bin/claude` but before postinstall runs,
/// in which the tool is PRESENT but is the stub — and execing it there failed a
/// peer pane's launch on 2026-10-21. A resolver treats the stub as a miss, so
/// whatever it does about an absent tool covers both windows. A native binary
/// starts with its ELF/PE/Mach-O magic, so the marker never matches one; an
/// unreadable file is NOT a placeholder (fail-open to the old behaviour).
/// Scope: npm on Linux/macOS, where `bin/claude` links straight to the
/// placeholder. On Windows npm's cmd-shim wrapper (`claude.cmd`) is what PATH
/// resolves, so the stub behind it is not seen and that window stays open.
pub fn is_npm_placeholder(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = Vec::with_capacity(PLACEHOLDER_PROBE_BYTES as usize);
    if file
        .take(PLACEHOLDER_PROBE_BYTES)
        .read_to_end(&mut head)
        .is_err()
    {
        return false;
    }
    head.windows(NPM_PLACEHOLDER_MARKER.len())
        .any(|w| w == NPM_PLACEHOLDER_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_stub_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("claude");

        // The head of the file @anthropic-ai/claude-code 2.1.314 ships.
        std::fs::write(
            &f,
            "echo \"Error: claude native binary not installed.\" >&2\necho \"\" >&2\n",
        )
        .unwrap();
        assert!(is_npm_placeholder(&f));

        std::fs::write(&f, b"\x7fELF\x02\x01\x01 native code").unwrap();
        assert!(!is_npm_placeholder(&f), "a native binary");

        std::fs::write(&f, "#!/bin/sh\nexec node cli.js \"$@\"\n").unwrap();
        assert!(
            !is_npm_placeholder(&f),
            "a wrapper script is a real launcher"
        );

        let mut far = vec![b'#'; PLACEHOLDER_PROBE_BYTES as usize];
        far.extend_from_slice(NPM_PLACEHOLDER_MARKER);
        std::fs::write(&f, far).unwrap();
        assert!(!is_npm_placeholder(&f), "the marker past the probed head");

        assert!(
            !is_npm_placeholder(&tmp.path().join("absent")),
            "a file that cannot be opened is not a placeholder"
        );
    }
}
