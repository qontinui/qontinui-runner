//! Is this file a real native executable image? The ONE definition of "a binary
//! the OS loader will actually run", used wherever the runner is about to hand
//! a binary on — today the identity-shim materializer
//! (`install_effects_producer::intercept::shim_materializer`), which decides
//! whether the `qontinui-pr` session CLI and the `qontinui-shim` stub beside the
//! runner exe may be published onto every runner terminal's PATH.
//!
//! WHY a format check and not just `len() > 0`: the defect this closes
//! (plan `2026-09-27-qontinui-pr-zero-byte-sidecar-placeholder-published-as-session-cli`,
//! coord dossier `qontinui-pr-shim-zero-byte-opens-no-pr`) was a 0-byte
//! `qontinui-pr.exe` on PATH. Git Bash runs a non-PE file with the execute bit
//! as a shell SCRIPT (`ENOEXEC` fallback), and an empty script exits 0, so
//! `qontinui-pr create` exited 0 having opened no PR. A non-empty text
//! placeholder would do the same with extra steps. Only a file carrying the
//! platform's executable magic is something the OS loader will actually run.

use std::io::Read;
use std::path::Path;

/// The executable image format a platform's loader runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutableFormat {
    /// Windows Portable Executable (`MZ` DOS header).
    Pe,
    /// Linux / other unix ELF (`\x7fELF`).
    Elf,
    /// macOS Mach-O, thin (32/64-bit, either byte order) or universal ("fat").
    MachO,
}

impl ExecutableFormat {
    /// The format the RUNNING platform's loader runs — what the runner may
    /// publish onto a terminal's PATH.
    pub fn host() -> Self {
        if cfg!(windows) {
            Self::Pe
        } else if cfg!(target_vendor = "apple") {
            Self::MachO
        } else {
            Self::Elf
        }
    }

    /// Does `head` (the file's first bytes) carry this format's magic?
    pub fn matches(self, head: &[u8]) -> bool {
        match self {
            Self::Pe => head.starts_with(b"MZ"),
            Self::Elf => head.starts_with(b"\x7fELF"),
            Self::MachO => matches!(
                head.get(..4),
                Some(
                    [0xfe, 0xed, 0xfa, 0xce] // MH_MAGIC (32-bit, big-endian)
                        | [0xfe, 0xed, 0xfa, 0xcf] // MH_MAGIC_64 (big-endian)
                        | [0xce, 0xfa, 0xed, 0xfe] // MH_CIGAM (32-bit, little-endian)
                        | [0xcf, 0xfa, 0xed, 0xfe] // MH_CIGAM_64 (little-endian)
                        | [0xca, 0xfe, 0xba, 0xbe] // FAT_MAGIC (universal)
                        | [0xbe, 0xba, 0xfe, 0xca] // FAT_CIGAM
                )
            ),
        }
    }
}

impl std::fmt::Display for ExecutableFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Pe => "a PE (MZ) executable",
            Self::Elf => "an ELF executable",
            Self::MachO => "a Mach-O executable",
        })
    }
}

/// Why a path is not a runnable native executable. `Missing` is listed first
/// because it is the one callers routinely treat as an honest state rather
/// than a defect: an absent CLI is "command not found", which falls through
/// PATH loudly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotExecutable {
    /// Nothing at that path.
    Missing,
    /// Something is there, but it is a directory or other non-file.
    NotAFile,
    /// A zero-length file — the build-placeholder shape that exits 0 silently.
    Empty,
    /// Non-empty, but not the expected image format (a script, text, or a
    /// binary for another platform).
    WrongFormat {
        len: u64,
        expected: ExecutableFormat,
    },
    /// Unix only: a real image with no execute bit — PATH lookup skips it.
    NoExecutePermission,
    /// The metadata or the header could not be read.
    Unreadable(String),
}

impl std::fmt::Display for NotExecutable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("no file at that path"),
            Self::NotAFile => f.write_str("not a regular file"),
            Self::Empty => f.write_str("zero-length file (a build placeholder, not a binary)"),
            Self::WrongFormat { len, expected } => {
                write!(f, "{len}-byte file that is not {expected}")
            }
            Self::NoExecutePermission => f.write_str("no execute permission"),
            Self::Unreadable(e) => write!(f, "unreadable: {e}"),
        }
    }
}

/// How many leading bytes the format check reads, and the fewest a file must
/// have to pass it. Four covers every magic above. PE's own magic is only the
/// two bytes `MZ`, so a 2- or 3-byte file starting `MZ` would match it; no real
/// image is that short, so [`check`] refuses anything under four bytes before
/// it looks at the magic at all.
const MAGIC_LEN: usize = 4;

/// Check that `path` is a regular, non-empty file carrying `format`'s magic
/// (and, on unix, at least one execute bit). Returns the file's length.
///
/// Follows symlinks (`metadata`, not `symlink_metadata`): a link to a real
/// binary is as runnable as the binary. Reads at most [`MAGIC_LEN`] bytes.
pub fn check(path: &Path, format: ExecutableFormat) -> Result<u64, NotExecutable> {
    let md = match std::fs::metadata(path) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(NotExecutable::Missing),
        Err(e) => return Err(NotExecutable::Unreadable(e.to_string())),
    };
    if !md.is_file() {
        return Err(NotExecutable::NotAFile);
    }
    let len = md.len();
    if len == 0 {
        return Err(NotExecutable::Empty);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if md.permissions().mode() & 0o111 == 0 {
            return Err(NotExecutable::NoExecutePermission);
        }
    }
    let mut head = Vec::with_capacity(MAGIC_LEN);
    let read =
        std::fs::File::open(path).and_then(|f| f.take(MAGIC_LEN as u64).read_to_end(&mut head));
    if let Err(e) = read {
        return Err(NotExecutable::Unreadable(e.to_string()));
    }
    if head.len() < MAGIC_LEN || !format.matches(&head) {
        return Err(NotExecutable::WrongFormat {
            len,
            expected: format,
        });
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write `bytes` to a fresh file under `dir` and (unix) mark it executable.
    fn file(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    // ---- magic literals, spelled out so a changed constant reddens a test ----

    #[test]
    fn pe_magic_is_mz() {
        assert!(ExecutableFormat::Pe.matches(b"MZ\x90\x00"));
        assert!(!ExecutableFormat::Pe.matches(b"\x7fELF"));
        assert!(!ExecutableFormat::Pe.matches(b"M"));
    }

    #[test]
    fn elf_magic_is_7f_elf() {
        assert!(ExecutableFormat::Elf.matches(b"\x7fELF\x02\x01"));
        assert!(!ExecutableFormat::Elf.matches(b"MZ\x90\x00"));
        assert!(!ExecutableFormat::Elf.matches(b"\x7fEL"));
    }

    #[test]
    fn macho_accepts_thin_and_universal_magics() {
        for magic in [
            [0xfe, 0xed, 0xfa, 0xce],
            [0xfe, 0xed, 0xfa, 0xcf],
            [0xce, 0xfa, 0xed, 0xfe],
            [0xcf, 0xfa, 0xed, 0xfe],
            [0xca, 0xfe, 0xba, 0xbe],
            [0xbe, 0xba, 0xfe, 0xca],
        ] {
            assert!(ExecutableFormat::MachO.matches(&magic), "{magic:02x?}");
        }
        assert!(!ExecutableFormat::MachO.matches(b"\x7fELF"));
        assert!(!ExecutableFormat::MachO.matches(&[0xcf, 0xfa, 0xed]));
    }

    #[test]
    fn a_shebang_script_is_no_platforms_executable() {
        for f in [
            ExecutableFormat::Pe,
            ExecutableFormat::Elf,
            ExecutableFormat::MachO,
        ] {
            assert!(!f.matches(b"#!/bin/sh\nexit 0\n"), "{f:?}");
        }
    }

    #[test]
    fn host_format_matches_the_compiling_platform() {
        let want = if cfg!(windows) {
            ExecutableFormat::Pe
        } else if cfg!(target_vendor = "apple") {
            ExecutableFormat::MachO
        } else {
            ExecutableFormat::Elf
        };
        assert_eq!(ExecutableFormat::host(), want);
    }

    // ---- check(): every arm ----

    #[test]
    fn check_accepts_a_file_with_the_expected_magic() {
        let tmp = tempfile::tempdir().unwrap();
        let p = file(tmp.path(), "real", b"\x7fELF\x02\x01\x01\x00payload");
        assert_eq!(check(&p, ExecutableFormat::Elf), Ok(15));
    }

    #[test]
    fn check_refuses_a_missing_path() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            check(&tmp.path().join("absent"), ExecutableFormat::Pe),
            Err(NotExecutable::Missing)
        );
    }

    #[test]
    fn check_refuses_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            check(tmp.path(), ExecutableFormat::Pe),
            Err(NotExecutable::NotAFile)
        );
    }

    #[test]
    fn check_refuses_a_zero_length_file() {
        // The exact shape of the defect: build.rs's placeholder, copied by
        // tauri-build, then published onto PATH.
        let tmp = tempfile::tempdir().unwrap();
        let p = file(tmp.path(), "placeholder.exe", b"");
        assert_eq!(check(&p, ExecutableFormat::Pe), Err(NotExecutable::Empty));
    }

    #[test]
    fn check_refuses_a_file_shorter_than_any_magic() {
        let tmp = tempfile::tempdir().unwrap();
        let p = file(tmp.path(), "short", b"M");
        assert_eq!(
            check(&p, ExecutableFormat::Pe),
            Err(NotExecutable::WrongFormat {
                len: 1,
                expected: ExecutableFormat::Pe
            })
        );
    }

    #[test]
    fn check_refuses_a_bare_pe_magic_too_short_to_be_an_image() {
        // `MZ` alone satisfies `ExecutableFormat::Pe::matches`; `check` must
        // not, or a 2-byte file would be published as the CLI.
        let tmp = tempfile::tempdir().unwrap();
        let p = file(tmp.path(), "mz", b"MZ");
        assert_eq!(
            check(&p, ExecutableFormat::Pe),
            Err(NotExecutable::WrongFormat {
                len: 2,
                expected: ExecutableFormat::Pe
            })
        );
    }

    #[test]
    fn check_refuses_a_script() {
        let tmp = tempfile::tempdir().unwrap();
        let p = file(tmp.path(), "script", b"#!/bin/sh\nexit 0\n");
        assert!(matches!(
            check(&p, ExecutableFormat::host()),
            Err(NotExecutable::WrongFormat { len: 17, .. })
        ));
    }

    #[test]
    fn check_refuses_another_platforms_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let p = file(tmp.path(), "elf", b"\x7fELF\x02\x01\x01\x00");
        assert!(matches!(
            check(&p, ExecutableFormat::Pe),
            Err(NotExecutable::WrongFormat {
                expected: ExecutableFormat::Pe,
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn check_refuses_a_real_image_without_an_execute_bit() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let p = file(tmp.path(), "noexec", b"\x7fELF\x02\x01\x01\x00");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            check(&p, ExecutableFormat::Elf),
            Err(NotExecutable::NoExecutePermission)
        );
    }

    #[test]
    fn rejections_name_their_reason() {
        assert!(NotExecutable::Empty.to_string().contains("zero-length"));
        let wrong = NotExecutable::WrongFormat {
            len: 17,
            expected: ExecutableFormat::Pe,
        };
        assert_eq!(
            wrong.to_string(),
            "17-byte file that is not a PE (MZ) executable"
        );
    }
}
