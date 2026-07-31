//! Owner-only file permissions for credential-bearing files (plan
//! 2026-07-17 runner credential hygiene, Phase 2 Task 6).
//!
//! The runner writes several files that carry live credentials or
//! credential-adjacent secrets to disk:
//!
//! - `.mcp.json` (workdir + repo root) — the per-session coord-mcp proxy nonce
//! - `~/.qontinui/runner/session-restore/coord-mcp/*.json` — the same nonce,
//!   app-data `--mcp-config` delivery
//! - `auth_tokens.enc` — the encrypted device-JWT / Cognito / machine-key store
//!
//! On Unix the default umask leaves these world-readable (`0644`); on Windows
//! they inherit the user-profile ACLs (already not world-readable, but not
//! owner-only either). This module provides one platform-differentiated
//! helper pair so every credential write lands owner-only:
//!
//! - Unix: `0600` via [`std::os::unix::fs::PermissionsExt`].
//! - Windows: an explicit protected DACL with a single ACE granting the
//!   process-token owner full access (native `windows-sys` call — no `icacls`
//!   subprocess). Inheritance is disabled so a permissive parent-directory
//!   ACL can never widen the file again.
//!
//! Failures are surfaced as `io::Error` so callers can choose their policy;
//! the credential writers treat a failure as warn-and-continue (the write
//! itself must not be lost over a hardening step — see call sites).

use std::io;
use std::path::Path;

/// Restrict an EXISTING file to owner-only access.
///
/// Unix: `chmod 0600`. Windows: replace the file's DACL with a protected
/// (non-inheriting) DACL containing exactly one ACE — full access for the
/// current process token's owner SID.
pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
    imp::restrict_to_owner(path)
}

/// Restrict an EXISTING directory to owner-only access, INHERITABLY — files
/// created inside it later are owner-only from birth.
///
/// This is the directory counterpart of [`restrict_to_owner`], and it is what
/// lets a credential writer restrict the containing directory BEFORE the write,
/// so the credential is never even briefly readable inside a permissive parent.
///
/// Unix: `chmod 0700`. Windows: the same protected single-ACE DACL as
/// [`restrict_to_owner`], but with `OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE`
/// so the ACE propagates to new children.
pub fn restrict_dir_to_owner(path: &Path) -> io::Result<()> {
    imp::restrict_dir_to_owner(path)
}

/// Write `contents` to `path` such that the file ends up owner-only.
///
/// Unix: the file is CREATED with mode `0600` (`OpenOptions::mode`), so there
/// is no window where the content exists world-readable; a pre-existing file's
/// mode is additionally tightened after the write (a plain rewrite would keep
/// the old, possibly-wider mode). Windows: write then apply the owner-only
/// DACL (the interim window is already covered by the user-profile ACL).
pub fn write_owner_only(path: &Path, contents: &[u8]) -> io::Result<()> {
    imp::write_owner_only(path, contents)
}

#[cfg(unix)]
mod imp {
    use std::io::{self, Write};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::Path;

    pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }

    pub fn restrict_dir_to_owner(path: &Path) -> io::Result<()> {
        // 0700, not 0600: a directory needs +x to be traversable by its owner.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }

    pub fn write_owner_only(path: &Path, contents: &[u8]) -> io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(contents)?;
        drop(f);
        // `mode(0o600)` only applies on CREATE — a pre-existing file keeps its
        // old mode through the rewrite, so tighten it explicitly.
        restrict_to_owner(path)
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::Security::Authorization::{SetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        AddAccessAllowedAceEx, GetLengthSid, GetTokenInformation, InitializeAcl, TokenUser,
        ACL as WIN_ACL, ACL_REVISION, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION,
        OBJECT_INHERIT_ACE, PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// The current process token's owner SID, as an owned byte buffer (the
    /// SID lives inside the returned `TOKEN_USER` blob; keep the whole blob
    /// alive and hand out the SID pointer's offset).
    fn process_owner_sid_blob() -> io::Result<Vec<u8>> {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            // First call sizes the buffer, second fills it.
            let mut needed: u32 = 0;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
            if needed == 0 {
                CloseHandle(token);
                return Err(io::Error::other("GetTokenInformation sizing returned 0"));
            }
            let mut buf = vec![0u8; needed as usize];
            let ok = GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr().cast(),
                needed,
                &mut needed,
            );
            CloseHandle(token);
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(buf)
        }
    }

    pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
        set_owner_only_dacl(path, 0)
    }

    pub fn restrict_dir_to_owner(path: &Path) -> io::Result<()> {
        // Propagate the ACE to children so files written into the directory
        // later are owner-only from birth (the `icacls (OI)(CI)` equivalent).
        set_owner_only_dacl(path, OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE)
    }

    /// Replace `path`'s DACL with a protected, single-ACE DACL granting full
    /// access to the current process token's owner SID. `ace_flags` carries the
    /// inheritance bits (0 for a file, OI|CI for a directory).
    fn set_owner_only_dacl(path: &Path, ace_flags: u32) -> io::Result<()> {
        use std::os::windows::ffi::OsStrExt;

        let blob = process_owner_sid_blob()?;
        // SAFETY: `blob` is a valid TOKEN_USER structure returned by
        // GetTokenInformation; its `User.Sid` points INTO the same blob, which
        // stays alive for the rest of this function.
        let sid = unsafe { (*(blob.as_ptr() as *const TOKEN_USER)).User.Sid };

        unsafe {
            let sid_len = GetLengthSid(sid);
            // ACL layout: header + one ACCESS_ALLOWED_ACE. The ACE struct
            // already embeds the first 4 SID bytes in `SidStart`, hence the
            // canonical `+ sid_len - 4` sizing from the Win32 docs.
            let ace_size = std::mem::size_of::<windows_sys::Win32::Security::ACCESS_ALLOWED_ACE>()
                as u32
                + sid_len
                - 4;
            let acl_size = std::mem::size_of::<WIN_ACL>() as u32 + ace_size;
            // DWORD-align per InitializeAcl requirements.
            let acl_size = (acl_size + 3) & !3;
            let mut acl_buf = vec![0u8; acl_size as usize];
            let acl_ptr = acl_buf.as_mut_ptr() as *mut WIN_ACL;
            if InitializeAcl(acl_ptr, acl_size, ACL_REVISION) == 0 {
                return Err(io::Error::last_os_error());
            }
            if AddAccessAllowedAceEx(acl_ptr, ACL_REVISION, ace_flags, FILE_ALL_ACCESS, sid) == 0 {
                return Err(io::Error::last_os_error());
            }

            let wide: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // PROTECTED_DACL_SECURITY_INFORMATION disables inheritance so the
            // parent directory's (wider) ACEs stop applying to this file.
            let rc = SetNamedSecurityInfoW(
                wide.as_ptr() as *mut u16,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl_ptr,
                std::ptr::null_mut(),
            );
            if rc != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
        }
        Ok(())
    }

    pub fn write_owner_only(path: &Path, contents: &[u8]) -> io::Result<()> {
        std::fs::write(path, contents)?;
        restrict_to_owner(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("qontinui-fs-perms-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{tag}-{}.txt", uuid::Uuid::new_v4()))
    }

    /// Cross-platform smoke: the write lands, content round-trips, and the
    /// restrict call succeeds (the platform-specific mode/DACL assertions are
    /// gated below).
    #[test]
    fn write_owner_only_writes_content_and_restricts() {
        let path = temp_file("smoke");
        write_owner_only(&path, b"secret").expect("write_owner_only");
        assert_eq!(std::fs::read(&path).unwrap(), b"secret");
        restrict_to_owner(&path).expect("restrict_to_owner on an existing file");
        let _ = std::fs::remove_file(&path);
    }

    /// Self-lockout regression (ported from the `coord_mcp` helper this module
    /// replaced): restricting must not cost the OWNER their own read. On
    /// Windows the DACL is not readable through `std::fs::Permissions`, so
    /// read-back is the assertion that actually matters on both platforms — a
    /// restriction that broke our own read would break every session spawn.
    #[test]
    fn restricting_does_not_lock_the_owner_out() {
        let path = temp_file("lockout");
        std::fs::write(&path, r#"{"nonce":"secret"}"#).unwrap();
        restrict_to_owner(&path).expect("restrict_to_owner");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"nonce":"secret"}"#,
            "restricting must not cost the owner their own read"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A directory restricted with [`restrict_dir_to_owner`] must stay
    /// traversable + writable by its owner — the credential writers restrict
    /// the directory BEFORE writing into it, so a too-tight mode here would
    /// fail the very write it exists to protect.
    #[test]
    fn restrict_dir_to_owner_keeps_the_dir_usable_by_its_owner() {
        let dir = std::env::temp_dir()
            .join("qontinui-fs-perms-tests")
            .join(format!("dir-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        restrict_dir_to_owner(&dir).expect("restrict_dir_to_owner");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o700,
                "credential dir must be owner-only, got {mode:o}"
            );
        }

        let inner = dir.join("cred.json");
        write_owner_only(&inner, b"secret").expect("write into a restricted dir");
        assert_eq!(std::fs::read(&inner).unwrap(), b"secret");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Missing path: the helpers must surface an `Err`, never panic. The
    /// credential writers treat hardening as advisory and keep delivering
    /// coord-mcp regardless, which only works if the failure is returnable.
    #[test]
    fn restricting_a_missing_path_errors_rather_than_panicking() {
        let missing =
            std::env::temp_dir().join(format!("fs-perms-absent-{}", uuid::Uuid::new_v4()));
        assert!(restrict_to_owner(&missing).is_err());
        assert!(restrict_dir_to_owner(&missing).is_err());
    }

    /// Task 6 acceptance (Unix): a credential file written through
    /// `write_owner_only` has mode `0600` — including a PRE-EXISTING file that
    /// started wider (the rewrite must tighten it, not inherit the old mode).
    #[cfg(unix)]
    #[test]
    fn write_owner_only_sets_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_file("mode");
        // Pre-existing WIDE file: default-ish 0644.
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_owner_only(&path, b"new-secret").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "credential file must be owner-only after write_owner_only"
        );
        let _ = std::fs::remove_file(&path);
    }
}
