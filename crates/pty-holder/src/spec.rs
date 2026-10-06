//! The child spec: what a holder runs on its PTY, and how it gets there.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 2. A holder is started with its pane's child — argv, cwd, the COMPLETE
//! environment, and the initial size.
//!
//! ## Delivery: a 0600 file in the pane's private directory, consumed on read
//!
//! The environment carries credentials the child legitimately needs, so it must
//! not travel on the holder's argv (`/proc/<pid>/cmdline` is world-readable on
//! Linux, and Windows exposes the command line to any same-session process).
//! The runner writes `<pane-dir>/<pane-id>.spec` instead ([`write_spec`]):
//! created `O_EXCL` with mode 0600 (Unix) inside the pane directory, which is
//! itself 0700 and ours ([`crate::pane::prepare_private_dir`]). The holder
//! opens it without following a symlink, refuses it unless it is a regular file
//! owned by its own uid with no group/other bits, reads it, and UNLINKS it
//! before it does anything else with it ([`consume_spec`]) — so the secret is on
//! disk only between the runner's write and the holder's start-up, in a
//! directory nobody else can enter. The spawner also removes it on every
//! failure path (`crate::spawn`).
//!
//! Why a file and not stdin: stdin cannot reach a holder spawned through WMI
//! `Win32_Process.Create` (plan D4's last-resort Windows route, Phase 3), and a
//! file works identically on every spawn route — plain, `systemd-run --scope`,
//! breakaway and WMI.
//!
//! ## The environment is COMPLETE, never inherited (plan D6)
//!
//! [`ChildSpec::env`] is the child's whole environment. The holder clears its
//! own before spawning the child and sets exactly these pairs, so nothing in
//! the holder's environment (which the spawner trims to a short allowlist
//! anyway) leaks into the pane. The credential SCRUB itself happens in the
//! runner, which can build a spec only from a `ScrubbedCommand` (D6: "the
//! holder's spawn path builds its child through `ScrubbedCommand::seal` — the
//! proof travels in the type"); this crate cannot see the runner's scrub list
//! and does not restate it.
//!
//! ## Byte fidelity
//!
//! argv, cwd and env are OS strings, which on Unix need not be UTF-8. They are
//! carried as [`WireOs`]: UTF-8 when they are, the platform's raw form
//! otherwise — never a lossy text conversion. DATA-PATH module: `source_guard`
//! bans text decoding here.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pane::{prepare_private_dir, spec_path, PaneId};

/// Largest spec file a holder will read: a bound on what a planted or corrupt
/// file can make it allocate. Real specs are a few KiB of environment.
pub const MAX_SPEC_BYTES: u64 = 1024 * 1024;

/// Default output ring size: what a reattach can still resume from.
pub const DEFAULT_RING_CAPACITY: usize = 2 * 1024 * 1024;
/// Smallest ring a spec may ask for.
pub const MIN_RING_CAPACITY: usize = 4 * 1024;

/// Default for how long a holder whose child has exited waits for a client to
/// collect the `exit` frame before it exits anyway (see `crate::pty`).
pub const DEFAULT_EXIT_LINGER_MS: u64 = 10 * 60 * 1000;

/// One OS string on the wire. `Utf8` whenever the string is valid UTF-8 (the
/// overwhelmingly common case, and readable in the file); otherwise the
/// platform's raw units, so nothing is lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireOs {
    Utf8(String),
    /// Unix: the raw bytes of a non-UTF-8 `OsStr`.
    UnixBytes(Vec<u8>),
    /// Windows: the raw UTF-16 units of an `OsStr` that is not valid Unicode.
    WindowsWide(Vec<u16>),
}

impl WireOs {
    pub fn from_os(s: &OsStr) -> WireOs {
        if let Some(t) = s.to_str() {
            return WireOs::Utf8(t.to_owned());
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            WireOs::UnixBytes(s.as_bytes().to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            WireOs::WindowsWide(s.encode_wide().collect())
        }
    }

    /// Back to an OS string. A raw form from the OTHER platform is an error,
    /// never a guess.
    pub fn to_os(&self) -> io::Result<OsString> {
        match self {
            WireOs::Utf8(s) => Ok(OsString::from(s.clone())),
            #[cfg(unix)]
            WireOs::UnixBytes(b) => {
                use std::os::unix::ffi::OsStringExt;
                Ok(OsString::from_vec(b.clone()))
            }
            #[cfg(windows)]
            WireOs::WindowsWide(w) => {
                use std::os::windows::ffi::OsStringExt;
                Ok(OsString::from_wide(w))
            }
            #[allow(unreachable_patterns)]
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "spec carries a raw OS string from another platform",
            )),
        }
    }
}

/// What a holder runs on its PTY.
///
/// `env` is the child's scrubbed environment and can still carry credentials,
/// so `Debug` is written by hand and REDACTS it (values and names): a spec —
/// or a `spawn::SpawnRequest` holding one — can be logged without printing
/// it. On disk the spec lives only as `<pane-id>.spec`, mode 0600 in the 0700
/// pane directory, from [`write_spec`] until the holder opens it
/// ([`consume_spec`] unlinks it before parsing); the spawner unlinks it after
/// EVERY attempt as well (`spawn::spawn_holder`), which covers a holder that
/// never started, exited on a held lock, or failed before reading it.
#[derive(Clone, PartialEq, Eq)]
pub struct ChildSpec {
    /// Program and arguments. Empty means the platform's default shell.
    pub argv: Vec<OsString>,
    /// Working directory; `None` inherits the holder's (which is wherever the
    /// spawner ran it — always say it explicitly in production).
    pub cwd: Option<OsString>,
    /// The child's COMPLETE environment. Nothing is inherited.
    pub env: Vec<(OsString, OsString)>,
    pub rows: u16,
    pub cols: u16,
    /// Output ring size; `None` is [`DEFAULT_RING_CAPACITY`]. Clamped up to
    /// [`MIN_RING_CAPACITY`].
    pub ring_capacity: Option<usize>,
    /// See [`DEFAULT_EXIT_LINGER_MS`]; `None` is that default.
    pub exit_linger_ms: Option<u64>,
}

impl std::fmt::Debug for ChildSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildSpec")
            .field("argv", &self.argv)
            .field("cwd", &self.cwd)
            .field(
                "env",
                &format_args!("<{} variables redacted>", self.env.len()),
            )
            .field("rows", &self.rows)
            .field("cols", &self.cols)
            .field("ring_capacity", &self.ring_capacity)
            .field("exit_linger_ms", &self.exit_linger_ms)
            .finish()
    }
}

impl ChildSpec {
    /// A spec for `argv` with an EMPTY environment and an 80×24 PTY.
    pub fn new(argv: Vec<OsString>) -> Self {
        ChildSpec {
            argv,
            cwd: None,
            env: Vec::new(),
            rows: 24,
            cols: 80,
            ring_capacity: None,
            exit_linger_ms: None,
        }
    }

    pub fn ring_capacity(&self) -> usize {
        self.ring_capacity
            .unwrap_or(DEFAULT_RING_CAPACITY)
            .max(MIN_RING_CAPACITY)
    }

    pub fn exit_linger(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.exit_linger_ms.unwrap_or(DEFAULT_EXIT_LINGER_MS))
    }
}

/// The spec file's JSON. Strict: the runner that writes it and the holder that
/// reads it are the same build (the holder ships beside the runner), so an
/// unknown field is a corrupt or foreign file, not a newer writer.
// No `Debug`: it holds the environment (see `ChildSpec`'s redacting `Debug`).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecWire {
    format: u32,
    argv: Vec<WireOs>,
    cwd: Option<WireOs>,
    env: Vec<(WireOs, WireOs)>,
    rows: u16,
    cols: u16,
    ring_capacity: Option<usize>,
    exit_linger_ms: Option<u64>,
}

const SPEC_FORMAT: u32 = 1;

fn to_wire(spec: &ChildSpec) -> SpecWire {
    SpecWire {
        format: SPEC_FORMAT,
        argv: spec.argv.iter().map(|a| WireOs::from_os(a)).collect(),
        cwd: spec.cwd.as_deref().map(WireOs::from_os),
        env: spec
            .env
            .iter()
            .map(|(k, v)| (WireOs::from_os(k), WireOs::from_os(v)))
            .collect(),
        rows: spec.rows,
        cols: spec.cols,
        ring_capacity: spec.ring_capacity,
        exit_linger_ms: spec.exit_linger_ms,
    }
}

fn from_wire(w: SpecWire) -> io::Result<ChildSpec> {
    if w.format != SPEC_FORMAT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("spec format {} (this build reads {SPEC_FORMAT})", w.format),
        ));
    }
    if w.rows == 0 || w.cols == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "spec PTY size must be at least 1x1",
        ));
    }
    let argv = w
        .argv
        .iter()
        .map(WireOs::to_os)
        .collect::<io::Result<Vec<_>>>()?;
    let cwd = w.cwd.as_ref().map(WireOs::to_os).transpose()?;
    let env = w
        .env
        .iter()
        .map(|(k, v)| Ok((k.to_os()?, v.to_os()?)))
        .collect::<io::Result<Vec<_>>>()?;
    Ok(ChildSpec {
        argv,
        cwd,
        env,
        rows: w.rows,
        cols: w.cols,
        ring_capacity: w.ring_capacity,
        exit_linger_ms: w.exit_linger_ms,
    })
}

/// Serialize a spec to the file's bytes.
pub fn encode_spec(spec: &ChildSpec) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&to_wire(spec)).map_err(io::Error::other)
}

/// Parse the file's bytes.
pub fn decode_spec(bytes: &[u8]) -> io::Result<ChildSpec> {
    let wire: SpecWire =
        serde_json::from_slice(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    from_wire(wire)
}

/// RUNNER side: write `<pane-dir>/<pane-id>.spec`, creating the pane directory
/// privately first. A stale spec from an earlier failed spawn is replaced; the
/// new file is created `O_EXCL` (never through a pre-planted symlink) with mode
/// 0600 on Unix. Returns the path, for the caller's cleanup.
pub fn write_spec(pane_dir: &Path, pane: &PaneId, spec: &ChildSpec) -> io::Result<PathBuf> {
    prepare_private_dir(pane_dir)?;
    let path = spec_path(pane_dir, pane);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let bytes = encode_spec(spec)?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&path)?;
    let written = f.write_all(&bytes).and_then(|()| f.flush());
    if let Err(e) = written {
        drop(f);
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(path)
}

/// HOLDER side: read the spec and unlink it. The file is removed as soon as it
/// is open — whether or not it then parses — so no copy of the child's
/// environment outlives the holder's start-up.
///
/// Unix refusals (`PermissionDenied`): a symlink (opened `O_NOFOLLOW`), not a
/// regular file, not owned by this uid, or any group/other permission bit.
pub fn consume_spec(pane_dir: &Path, pane: &PaneId) -> io::Result<ChildSpec> {
    let path = spec_path(pane_dir, pane);
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let f = opts.open(&path)?;
    // Unlink first: whatever happens below, the secret leaves the disk now.
    let _ = std::fs::remove_file(&path);
    let md = f.metadata()?;
    if !md.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "spec is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        if md.uid() != me {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("spec owned by uid {}, not {me}", md.uid()),
            ));
        }
        if md.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("spec mode {:o} is not private", md.mode() & 0o777),
            ));
        }
    }
    let mut bytes = Vec::new();
    f.take(MAX_SPEC_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SPEC_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("spec exceeds {MAX_SPEC_BYTES} bytes"),
        ));
    }
    decode_spec(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn tmp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ptyh-spec-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn sample() -> ChildSpec {
        ChildSpec {
            argv: vec!["sh".into(), "-c".into(), "echo hi".into()],
            cwd: Some("/tmp".into()),
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            rows: 30,
            cols: 100,
            ring_capacity: Some(8192),
            exit_linger_ms: Some(5),
        }
    }

    /// Review round 2, N9: `Debug` on a spec — and on a `SpawnRequest`
    /// carrying one — never prints the environment.
    #[test]
    fn pty_holder_spec_debug_redacts_the_environment() {
        let mut spec = sample();
        spec.env
            .push(("ANTHROPIC_API_KEY".into(), "sk-secret-value".into()));
        let shown = format!("{spec:?}");
        assert!(!shown.contains("sk-secret-value"), "{shown}");
        assert!(!shown.contains("ANTHROPIC_API_KEY"), "{shown}");
        assert!(shown.contains("2 variables redacted"), "{shown}");
        let pane = crate::pane::PaneId::new("p").unwrap();
        let req = crate::spawn::SpawnRequest {
            holder_exe: std::path::Path::new("/x"),
            pane_dir: std::path::Path::new("/y"),
            pane_id: &pane,
            child: &spec,
            route: crate::spawn::RouteRequest::Auto,
            report_timeout: std::time::Duration::from_secs(1),
        };
        assert!(!format!("{req:?}").contains("sk-secret-value"));
    }

    #[test]
    fn pty_holder_spec_round_trips() {
        let s = sample();
        assert_eq!(decode_spec(&encode_spec(&s).unwrap()).unwrap(), s);
    }

    /// A non-UTF-8 OS string survives byte-for-byte, carried raw.
    #[cfg(unix)]
    #[test]
    fn pty_holder_spec_non_utf8_os_strings_survive() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let raw: Vec<u8> = vec![b'a', 0xFF, 0xFE, 0x80, b'z'];
        let mut s = sample();
        s.argv.push(OsString::from_vec(raw.clone()));
        s.env.push((
            "K".into(),
            OsString::from_vec((0u8..=255).filter(|b| *b != 0).collect()),
        ));
        let back = decode_spec(&encode_spec(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.argv[3].as_bytes(), &raw[..]);
        assert!(matches!(
            WireOs::from_os(OsStr::from_bytes(&raw)),
            WireOs::UnixBytes(_)
        ));
    }

    #[test]
    fn pty_holder_spec_refuses_foreign_or_corrupt_files() {
        assert!(decode_spec(b"{}").is_err());
        assert!(decode_spec(b"\xff").is_err());
        let mut v: serde_json::Value =
            serde_json::from_slice(&encode_spec(&sample()).unwrap()).unwrap();
        v["surprise"] = serde_json::json!(1);
        assert!(decode_spec(&serde_json::to_vec(&v).unwrap()).is_err());
        let mut v: serde_json::Value =
            serde_json::from_slice(&encode_spec(&sample()).unwrap()).unwrap();
        v["format"] = serde_json::json!(99);
        assert!(decode_spec(&serde_json::to_vec(&v).unwrap()).is_err());
        let mut z = sample();
        z.rows = 0;
        assert!(decode_spec(&encode_spec(&z).unwrap()).is_err());
    }

    /// Written 0600, consumed once: the file is gone after the read.
    #[cfg(unix)]
    #[test]
    fn pty_holder_spec_file_is_private_and_consumed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir("file");
        let pane = PaneId::new("p1").unwrap();
        let path = write_spec(&dir, &pane, &sample()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(consume_spec(&dir, &pane).unwrap(), sample());
        assert!(!path.exists(), "consumed specs are unlinked");
        assert_eq!(
            consume_spec(&dir, &pane).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );

        // A group-readable spec is refused (and still removed).
        write_spec(&dir, &pane, &sample()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(
            consume_spec(&dir, &pane).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(!path.exists());

        // A symlink planted at the spec path is never followed.
        let target = dir.join("elsewhere");
        std::fs::write(&target, encode_spec(&sample()).unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(consume_spec(&dir, &pane).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
