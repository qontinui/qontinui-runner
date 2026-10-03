//! `coord.computers.gpus` — the static GPU inventory of a computer (plan
//! `2026-09-30-the-fleet-machine-is-not-a-first-class-coord-entity-and-coord-has-no-resource-model`,
//! amendment 2026-10-01 "the `gpus` column gets a shape, a publisher and a
//! consumer").
//!
//! ## Shape
//!
//! `[{vendor, model, vram_bytes, driver, compute_capability}]`, every member
//! but `vendor` nullable (an `[N/A]` from the driver is `null`, never a
//! fabricated zero):
//!
//! * **`[]` means MEASURED: no GPU.** It is published only when absence is
//!   POSITIVELY established — see [`decide`].
//! * **`null` means UNKNOWN.** Whenever measurement could not run, failed,
//!   timed out, or produced anything this parser does not fully understand.
//!   coord stores the column with `COALESCE`, so a `null` tick keeps the last
//!   reading rather than erasing it: a transient failure costs nothing.
//!
//! ## What is measured, and what is NOT
//!
//! Only **NVIDIA** GPUs are enumerated, through
//! `nvidia-smi --query-gpu=name,memory.total,driver_version,compute_cap
//! --format=csv,noheader,nounits` (`memory.total` in MiB). AMD, Intel and
//! every other vendor are **not measured** — so a non-empty list means "at
//! least these NVIDIA GPUs", never "only these GPUs". That is also why `[]`
//! needs more than a missing `nvidia-smi`: a box with an AMD or Intel GPU and
//! no NVIDIA one would otherwise be published as having no GPU at all.
//!
//! ## Where `[]` can be claimed
//!
//! Only on a Linux host that is not a WSL guest, and only when ALL of these
//! hold (read by [`absence_evidence_at`]; any unreadable check is UNKNOWN):
//!
//! * `nvidia-smi` is not installed (or ran and listed zero GPUs);
//! * no `/dev/nvidia*` node and no `/proc/driver/nvidia` — the NVIDIA kernel
//!   driver is not loaded;
//! * no `/dev/dxg` — a WSL2 guest reaches its GPUs only through that
//!   paravirtual device, where `/dev/nvidia*` never exists, so on WSL the
//!   two checks above prove nothing;
//! * no PCI device in `/sys/bus/pci/devices` is a possible GPU, read from PCI
//!   rather than DRM so a GPU with NO driver bound (a datacenter card before
//!   its driver is installed, a `vfio-pci` passthrough card, a `nomodeset`
//!   boot) still blocks `[]`. It fails CLOSED by class ([`pci_device_is_gpu`]):
//!   every 3D controller (`0x0302`) and processing accelerator (`0x12xxxx`)
//!   blocks, from any vendor; any other display controller (`0x03xxxx`)
//!   blocks unless its vendor is a known framebuffer-only one
//!   ([`FRAMEBUFFER_VENDORS`]: a server BMC's VGA, a VM's virtual VGA);
//! * every `/sys/class/drm` `card<N>` is from such a framebuffer-only vendor
//!   — a card with an unreadable vendor (a platform GPU, as on an ARM board)
//!   is UNKNOWN, as is an absent `/sys/class/drm`.
//!
//! **Windows and macOS hosts never publish `[]`**: an absent `nvidia-smi.exe`
//! there has no cheap, reliable corroboration, so it is UNKNOWN.
//!
//! ## WSL guests
//!
//! The Windows host lane measures for its guests (the amendment's "Windows
//! host lane for a WSL guest"): a WSL2 guest sees the host's GPUs SHARED
//! through GPU paravirtualization (`/dev/dxg`), not carved off as its RAM is,
//! and `wsl.exe` is already forked once per guest per full report, so the
//! probe only learns whether `/dev/dxg` exists ([`guest_gpus`]). A consumer
//! summing fleet VRAM must therefore skip `wsl_guest` rows (they carry a
//! `parent_identity_hash`) — the same physical GPU appears on both.
//!
//! A runner running INSIDE a guest reports that guest's row too, so it sends
//! `gpus: null` there (`super::host_report`) and leaves the row to the host
//! lane — one writer per row. It still advertises its own `gpu:*`
//! capabilities from its own reading: those describe where IT can run work.
//!
//! ## Bounds
//!
//! coord refuses a whole report whose `gpus` serializes past
//! [`super::MAX_JSON_BYTES`]. [`MAX_GPUS`], [`MAX_GPU_TEXT`] (BYTES) and
//! [`MAX_COMPUTE_CAP_LEN`] keep even a worst-case, escape-heavy reading under
//! ~11 KiB (pinned by a test); a machine with more GPUs than [`MAX_GPUS`] is
//! UNKNOWN rather than a truncated, understated list.
//!
//! ## Never block the caller for long
//!
//! One measurement is bounded by [`NVIDIA_SMI_TIMEOUT`] plus a 1 s reap grace,
//! after which a child a sick driver has wedged in the kernel is abandoned
//! rather than waited on. The cache is single-flight: a caller arriving while
//! a measurement is in progress gets the previous reading instead of starting
//! a second `nvidia-smi`.

use std::time::Duration;

use serde::Serialize;

/// One GPU on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Gpu {
    /// Lowercase vendor (`nvidia`). Always set: it is the measurement source.
    pub(crate) vendor: String,
    pub(crate) model: Option<String>,
    pub(crate) vram_bytes: Option<u64>,
    pub(crate) driver: Option<String>,
    /// CUDA compute capability as the driver prints it (`8.9`).
    pub(crate) compute_capability: Option<String>,
}

/// More GPUs than this is UNKNOWN (keeps the reading under coord's bound).
pub(crate) const MAX_GPUS: usize = 16;
/// A text member longer than this many BYTES is `null`, not truncated.
pub(crate) const MAX_GPU_TEXT: usize = 128;
/// A compute capability longer than this is malformed (`major.minor`).
pub(crate) const MAX_COMPUTE_CAP_LEN: usize = 7;
/// Wall-clock budget for one `nvidia-smi` run (it can wedge on a sick driver).
pub(crate) const NVIDIA_SMI_TIMEOUT: Duration = Duration::from_secs(5);
/// How long one host reading is reused by the reporter and the heartbeat.
pub(crate) const GPU_READING_TTL: Duration = Duration::from_secs(300);

/// The amendment's query, verbatim (`memory.total` in MiB under `nounits`).
const QUERY_ARGS: [&str; 2] = [
    "--query-gpu=name,memory.total,driver_version,compute_cap",
    "--format=csv,noheader,nounits",
];

/// What running `nvidia-smi` produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NvidiaReading {
    /// Exited 0 and every row parsed (possibly zero rows).
    Gpus(Vec<Gpu>),
    /// The binary is not installed (spawn → `NotFound`).
    Absent,
    /// Anything else: spawn error, non-zero exit (no device, driver not
    /// loaded), timeout, malformed output, too many GPUs.
    Failed,
}

/// The driver's "no value" spellings.
fn na(v: &str) -> bool {
    let v = v.trim();
    v.is_empty()
        || v.eq_ignore_ascii_case("[N/A]")
        || v.eq_ignore_ascii_case("N/A")
        || v.eq_ignore_ascii_case("[Not Supported]")
        || v.eq_ignore_ascii_case("[Unknown Error]")
        || v.eq_ignore_ascii_case("[Insufficient Permissions]")
}

fn text(v: &str) -> Option<String> {
    let v = v.trim();
    (!na(v) && v.len() <= MAX_GPU_TEXT && !v.chars().any(char::is_control)).then(|| v.to_string())
}

/// Parse one CSV row. `Err` when the row is not four fields, has no name, or
/// carries a value that is neither a number nor an `[N/A]` spelling where a
/// number belongs.
fn parse_row(line: &str) -> Result<Gpu, ()> {
    let f: Vec<&str> = line.split(',').map(str::trim).collect();
    let [name, mem, driver, cc] = f.as_slice() else {
        return Err(());
    };
    if na(name) {
        return Err(());
    }
    let vram_bytes = if na(mem) {
        None
    } else {
        let mib: u64 = mem.parse().map_err(|_| ())?;
        Some(mib.checked_mul(1024 * 1024).ok_or(())?)
    };
    let compute_capability = if na(cc) {
        None
    } else {
        // `major.minor`, digits only — anything else is not a value we
        // understand, and a wrong capability would mis-place CUDA work.
        let ok = cc.len() <= MAX_COMPUTE_CAP_LEN
            && cc.split_once('.').is_some_and(|(a, b)| {
                !a.is_empty()
                    && !b.is_empty()
                    && a.bytes().all(|c| c.is_ascii_digit())
                    && b.bytes().all(|c| c.is_ascii_digit())
            });
        if !ok {
            return Err(());
        }
        Some((*cc).to_string())
    };
    Ok(Gpu {
        vendor: "nvidia".into(),
        model: text(name),
        vram_bytes,
        driver: text(driver),
        compute_capability,
    })
}

/// Parse `nvidia-smi --query-gpu=… --format=csv,noheader,nounits` stdout from
/// a run that exited 0. PURE.
///
/// Blank lines are skipped. ANY malformed row makes the whole reading
/// [`NvidiaReading::Failed`]: dropping the row would publish an understated
/// list as if it were complete. More than [`MAX_GPUS`] rows is `Failed` too.
pub(crate) fn parse_nvidia_smi(stdout: &str) -> NvidiaReading {
    let mut gpus = Vec::new();
    for line in stdout
        .lines()
        .map(|l| l.trim_start_matches('\u{feff}').trim())
    {
        if line.is_empty() {
            continue;
        }
        match parse_row(line) {
            Ok(g) => gpus.push(g),
            Err(()) => return NvidiaReading::Failed,
        }
        if gpus.len() > MAX_GPUS {
            return NvidiaReading::Failed;
        }
    }
    NvidiaReading::Gpus(gpus)
}

/// What a Linux host can establish about GPU absence without `nvidia-smi`.
/// Every field is `None` when it could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct AbsenceEvidence {
    /// A `/dev/nvidia*` node or `/proc/driver/nvidia` exists.
    pub(crate) nvidia_driver_present: Option<bool>,
    /// `/dev/dxg` exists (a WSL2 guest's GPU paravirtualization device).
    pub(crate) wsl_gpu_paravirt: Option<bool>,
    /// A PCI device that may be a GPU exists ([`pci_device_is_gpu`]), driver
    /// bound or not.
    pub(crate) gpu_pci_device: Option<bool>,
    /// A DRM `card<N>` that is not from a framebuffer-only vendor exists;
    /// `None` when any card's vendor cannot be read (a platform GPU).
    pub(crate) gpu_class_drm_device: Option<bool>,
}

impl AbsenceEvidence {
    /// True only when every check answered and each says "nothing".
    pub(crate) fn proves_no_gpu(&self) -> bool {
        self.nvidia_driver_present == Some(false)
            && self.wsl_gpu_paravirt == Some(false)
            && self.gpu_pci_device == Some(false)
            && self.gpu_class_drm_device == Some(false)
    }
}

/// PCI vendors whose display controllers are framebuffers, never compute
/// GPUs: ASPEED and Matrox (server BMCs), Cirrus, and the virtual VGAs of
/// QEMU/Bochs, Red Hat (QXL), virtio, VMware, Hyper-V and VirtualBox. Every
/// OTHER vendor's display device is a possible GPU — the list fails closed.
pub(crate) const FRAMEBUFFER_VENDORS: [&str; 9] = [
    "0x1a03", "0x102b", "0x1013", "0x1234", "0x1b36", "0x1af4", "0x15ad", "0x1414", "0x80ee",
];

/// Parse a sysfs `vendor` file (`0x10de`). `None` for anything else. PURE.
fn pci_vendor(vendor_file: &str) -> Option<String> {
    let v = vendor_file.trim().to_ascii_lowercase();
    let hex = v.strip_prefix("0x")?;
    (hex.len() == 4 && hex.bytes().all(|c| c.is_ascii_hexdigit())).then_some(v)
}

/// Whether a display device of this vendor may be a GPU (anything but a
/// [`FRAMEBUFFER_VENDORS`] vendor). `None` for an unreadable vendor. PURE.
pub(crate) fn display_vendor_may_be_gpu(vendor_file: &str) -> Option<bool> {
    let v = pci_vendor(vendor_file)?;
    Some(!FRAMEBUFFER_VENDORS.contains(&v.as_str()))
}

/// Whether a PCI device's `class` and `vendor` sysfs files describe a
/// possible GPU, failing CLOSED: a 3D controller (`0x0302xx`) or processing
/// accelerator (`0x12xxxx`) of ANY vendor, or another display controller
/// (`0x03xxxx`) of a vendor that is not framebuffer-only. Every other class
/// (NICs, storage, bridges) is not. `None` when either file is not the
/// expected hex form. PURE.
pub(crate) fn pci_device_is_gpu(class_file: &str, vendor_file: &str) -> Option<bool> {
    let class = class_file.trim().to_ascii_lowercase();
    let hex = class.strip_prefix("0x")?;
    if hex.len() != 6 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let vendor_may_be_gpu = display_vendor_may_be_gpu(vendor_file)?;
    Some(if hex.starts_with("0302") || hex.starts_with("12") {
        true
    } else if hex.starts_with("03") {
        vendor_may_be_gpu
    } else {
        false
    })
}

/// Combine the NVIDIA reading with the absence evidence into the published
/// value. PURE — this is the whole `[]`-versus-`null` policy.
///
/// * GPUs listed → those GPUs.
/// * Zero listed, or not installed → `[]` only when `evidence` proves no GPU,
///   else `null`.
/// * Failed → `null`, always.
pub(crate) fn decide(
    reading: NvidiaReading,
    evidence: Option<AbsenceEvidence>,
) -> Option<Vec<Gpu>> {
    match reading {
        NvidiaReading::Gpus(g) if !g.is_empty() => Some(g),
        NvidiaReading::Gpus(_) | NvidiaReading::Absent => {
            evidence.is_some_and(|e| e.proves_no_gpu()).then(Vec::new)
        }
        NvidiaReading::Failed => None,
    }
}

/// A WSL guest's `gpus`, from the host's own reading and whether the guest
/// has the paravirtual GPU device. PURE.
///
/// * `/dev/dxg` present → the host's reading, as is (a `null` host stays
///   `null`).
/// * `/dev/dxg` absent → `[]`: a WSL2 guest has no other path to a GPU of
///   any vendor (GPU support is disabled for it).
/// * Not reported (an older probe, a failed line) → `null`.
pub(crate) fn guest_gpus(dxg: Option<bool>, host: Option<&[Gpu]>) -> Option<Vec<Gpu>> {
    match dxg? {
        true => host.map(<[Gpu]>::to_vec),
        false => Some(Vec::new()),
    }
}

/// Capability token: a CUDA-capable NVIDIA GPU is present.
pub(crate) const CUDA_CAPABILITY: &str = "gpu:cuda";
/// Capability token prefix: the largest single CUDA GPU's VRAM, in GiB.
pub(crate) const VRAM_CAPABILITY_PREFIX: &str = "gpu:vram:";

/// Capability tokens for `/coord/devices/register`. PURE.
///
/// A GPU counts as CUDA-capable only with a parsed compute capability. The
/// VRAM token names the LARGEST single CUDA GPU (one job runs on one GPU),
/// rounded to the NEAREST GiB so a card the vendor sells as 32 GB — which the
/// driver reports as ~31.8 GiB usable — advertises `gpu:vram:32`. coord
/// matches capability strings by containment, so a registration must name the
/// exact token; this spelling is the contract. Nothing is advertised for an
/// UNKNOWN reading (fail-closed: under-advertising only moves work elsewhere).
pub(crate) fn capability_tokens(gpus: Option<&[Gpu]>) -> Vec<String> {
    let cuda: Vec<&Gpu> = gpus
        .unwrap_or_default()
        .iter()
        .filter(|g| g.vendor == "nvidia" && g.compute_capability.is_some())
        .collect();
    if cuda.is_empty() {
        return Vec::new();
    }
    let mut out = vec![CUDA_CAPABILITY.to_string()];
    if let Some(max) = cuda.iter().filter_map(|g| g.vram_bytes).max() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let gib = max.saturating_add(GIB / 2) / GIB;
        if gib > 0 {
            out.push(format!("{VRAM_CAPABILITY_PREFIX}{gib}"));
        }
    }
    out
}

// ---- measurement (impure) ------------------------------------------------

/// How long a killed `nvidia-smi` is given to be reaped before it is
/// abandoned (a process wedged in a driver call ignores SIGKILL until the
/// call returns).
const REAP_GRACE: Duration = Duration::from_secs(1);

/// A killed `nvidia-smi` that did not exit within [`REAP_GRACE`] — kept so
/// it can be reaped later, and so NO new one is started while it lives: a
/// driver that wedged one call will wedge the next, and an unbounded pile of
/// stuck processes is worse than an UNKNOWN reading.
static ABANDONED: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

/// Whether an abandoned child is still alive (reaping it if it has exited).
fn abandoned_still_alive() -> bool {
    let Ok(mut g) = ABANDONED.lock() else {
        return true;
    };
    match g.as_mut().map(std::process::Child::try_wait) {
        None => false,
        Some(Ok(None)) => true,
        Some(_) => {
            *g = None;
            false
        }
    }
}

/// The most stdout a reading may have (16 rows need well under 8 KiB).
const MAX_STDOUT: u64 = 64 * 1024;

/// Run the query `cmd` under `timeout`. Polls `try_wait` (no thread, no task
/// — same shape as `fleet::command_succeeds_within`). A wedged child is
/// killed and, if it is still not reaped after [`REAP_GRACE`], parked in
/// [`ABANDONED`] — never waited on without bound, and never joined by a
/// second one. Output is read only after exit, so a child that fills its
/// pipe simply times out.
fn run_command(mut cmd: std::process::Command, timeout: Duration) -> NvidiaReading {
    use std::io::Read;
    if abandoned_still_alive() {
        tracing::debug!(
            "fleet::computer::gpu: an earlier nvidia-smi is still wedged — gpus UNKNOWN"
        );
        return NvidiaReading::Failed;
    }
    let started = std::time::Instant::now();
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return NvidiaReading::Absent,
        Err(e) => {
            tracing::debug!("fleet::computer::gpu: nvidia-smi spawn failed ({e}) — gpus UNKNOWN");
            return NvidiaReading::Failed;
        }
    };
    let give_up = |mut child: std::process::Child| {
        let _ = child.kill();
        let reap_by = std::time::Instant::now() + REAP_GRACE;
        while std::time::Instant::now() < reap_by {
            if !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        tracing::warn!(
            "fleet::computer::gpu: nvidia-smi did not exit after kill — parked; \
             no new measurement until it exits"
        );
        if let Ok(mut g) = ABANDONED.lock() {
            *g = Some(child);
        }
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    tracing::debug!(
                        "fleet::computer::gpu: nvidia-smi exited {:?} — gpus UNKNOWN",
                        status.code()
                    );
                    return NvidiaReading::Failed;
                }
                let mut buf = Vec::new();
                let read = child
                    .stdout
                    .take()
                    .map(|s| s.take(MAX_STDOUT + 1).read_to_end(&mut buf));
                // More than MAX_STDOUT is not a reading we would publish whole,
                // and a silent cut on a line boundary would parse as complete.
                if !matches!(read, Some(Ok(_))) || buf.len() as u64 > MAX_STDOUT {
                    return NvidiaReading::Failed;
                }
                return match std::str::from_utf8(&buf) {
                    Ok(s) => parse_nvidia_smi(s),
                    Err(_) => NvidiaReading::Failed,
                };
            }
            Ok(None) if started.elapsed() >= timeout => {
                tracing::warn!(
                    "fleet::computer::gpu: nvidia-smi exceeded {}s — gpus UNKNOWN",
                    timeout.as_secs()
                );
                give_up(child);
                return NvidiaReading::Failed;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => {
                give_up(child);
                return NvidiaReading::Failed;
            }
        }
    }
}

/// Run `program` with the amendment's query.
fn run_nvidia_smi(program: &std::path::Path, timeout: Duration) -> NvidiaReading {
    let mut cmd = crate::process_helpers::no_window(program);
    cmd.args(QUERY_ARGS);
    run_command(cmd, timeout)
}

/// Where `nvidia-smi` is looked for, in order: PATH (`Command` appends
/// `.exe` on Windows), then on Linux WSL's `/usr/lib/wsl/lib/nvidia-smi`,
/// which a systemd-launched runner's PATH usually lacks.
fn nvidia_smi_candidates() -> Vec<&'static std::path::Path> {
    let mut v = vec![std::path::Path::new("nvidia-smi")];
    if cfg!(target_os = "linux") {
        v.push(std::path::Path::new("/usr/lib/wsl/lib/nvidia-smi"));
    }
    v
}

/// The reading from the first candidate that is installed: a candidate whose
/// run says [`NvidiaReading::Absent`] falls through to the next one that
/// `exists`; any other answer is final. PURE given its two callbacks.
fn nvidia_reading_with(
    candidates: &[&std::path::Path],
    exists: impl Fn(&std::path::Path) -> bool,
    run: impl Fn(&std::path::Path) -> NvidiaReading,
) -> NvidiaReading {
    for (i, c) in candidates.iter().enumerate() {
        // The first candidate is a bare name resolved through PATH, so only
        // the absolute fallbacks are pre-checked.
        if i > 0 && !exists(c) {
            continue;
        }
        let r = run(c);
        if r != NvidiaReading::Absent {
            return r;
        }
    }
    NvidiaReading::Absent
}

fn nvidia_reading() -> NvidiaReading {
    nvidia_reading_with(&nvidia_smi_candidates(), std::path::Path::exists, |p| {
        run_nvidia_smi(p, NVIDIA_SMI_TIMEOUT)
    })
}

/// Every entry of `dir`, or `None` when the directory (absent included) or
/// ANY entry could not be read — a skipped entry, or a missing directory,
/// must never count as evidence of absence.
fn entries(dir: &std::path::Path) -> Option<Vec<std::fs::DirEntry>> {
    std::fs::read_dir(dir)
        .ok()?
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

/// Read the Linux absence evidence under `root` (`/` in production; a
/// temporary tree in tests). Every check that cannot be read is `None`.
pub(crate) fn absence_evidence_at(root: &std::path::Path) -> AbsenceEvidence {
    let exists = |p: &str| -> Option<bool> {
        match std::fs::symlink_metadata(root.join(p)) {
            Ok(_) => Some(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
            Err(_) => None,
        }
    };
    // `/dev` itself missing is not "no nodes": it is an unreadable check.
    let dev_nvidia = std::fs::read_dir(root.join("dev"))
        .ok()
        .and_then(|rd| rd.collect::<Result<Vec<_>, _>>().ok())
        .map(|v| {
            v.iter()
                .any(|e| e.file_name().to_string_lossy().starts_with("nvidia"))
        });
    let nvidia_driver_present = match (dev_nvidia, exists("proc/driver/nvidia")) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    };
    let read = |p: std::path::PathBuf| std::fs::read_to_string(p).ok();
    // An absent directory is UNKNOWN (no sysfs), never "no devices".
    let gpu_pci_device = entries(&root.join("sys/bus/pci/devices")).and_then(|es| {
        let mut any = false;
        for e in es {
            let class = read(e.path().join("class"))?;
            let vendor = read(e.path().join("vendor"))?;
            any |= pci_device_is_gpu(&class, &vendor)?;
        }
        Some(any)
    });
    let gpu_class_drm_device = entries(&root.join("sys/class/drm")).and_then(|es| {
        let mut any = false;
        for e in es {
            let name = e.file_name().to_string_lossy().into_owned();
            // `card0`, not `card0-HDMI-A-1` (a connector) or `renderD128`.
            let is_card = name
                .strip_prefix("card")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()));
            if !is_card {
                continue;
            }
            let vendor = read(e.path().join("device/vendor"))?;
            any |= display_vendor_may_be_gpu(&vendor)?;
        }
        Some(any)
    });
    AbsenceEvidence {
        nvidia_driver_present,
        wsl_gpu_paravirt: exists("dev/dxg"),
        gpu_pci_device,
        gpu_class_drm_device,
    }
}

/// The absence evidence on this host. `None` off Linux.
fn linux_absence_evidence() -> Option<AbsenceEvidence> {
    #[cfg(target_os = "linux")]
    {
        Some(absence_evidence_at(std::path::Path::new("/")))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Measure this host now. Blocking (forks `nvidia-smi`, reads sysfs).
pub(crate) fn measure_host() -> Option<Vec<Gpu>> {
    let reading = nvidia_reading();
    let evidence = if wants_absence_evidence(&reading) {
        linux_absence_evidence()
    } else {
        None
    };
    decide(reading, evidence)
}

/// Absence evidence is read only when NVIDIA was not installed or listed
/// nothing — never after a failure, whose answer is UNKNOWN regardless. PURE.
pub(crate) fn wants_absence_evidence(reading: &NvidiaReading) -> bool {
    match reading {
        NvidiaReading::Gpus(g) => g.is_empty(),
        NvidiaReading::Absent => true,
        NvidiaReading::Failed => false,
    }
}

/// How long the last KNOWN reading keeps backing the capability tokens while
/// the current one is UNKNOWN: one more reading period, so a single slow
/// `nvidia-smi` does not withdraw `gpu:cuda`, while a lasting failure does.
/// A WEDGED `nvidia-smi` withdraws them at once ([`host_gpus_for_capabilities`]):
/// that is exactly when CUDA work would fail.
pub(crate) const LAST_KNOWN_TTL: Duration = Duration::from_secs(2 * 300);

/// The shared host reading.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GpuCache {
    /// The latest reading (`None` = UNKNOWN).
    pub(crate) reading: Option<Vec<Gpu>>,
    /// When `reading` was taken — or when a measurement was STARTED, which
    /// is what makes the cache single-flight.
    pub(crate) at: std::time::Instant,
    /// The latest non-UNKNOWN reading and when it was taken.
    pub(crate) last_known: Option<(Vec<Gpu>, std::time::Instant)>,
}

impl GpuCache {
    /// Whether a new measurement is due at `now`. PURE.
    pub(crate) fn due(cache: Option<&Self>, now: std::time::Instant) -> bool {
        cache.is_none_or(|c| now.saturating_duration_since(c.at) >= GPU_READING_TTL)
    }

    /// Claim the measurement at `now` if one is due: returns the new state
    /// (with `at` moved to `now`, so later callers see it as fresh and keep
    /// the previous reading) and whether THIS caller must measure. PURE.
    pub(crate) fn claim(cache: Option<Self>, now: std::time::Instant) -> (Self, bool) {
        if !Self::due(cache.as_ref(), now) {
            if let Some(c) = cache {
                return (c, false);
            }
        }
        let mut c = cache.unwrap_or(GpuCache {
            reading: None,
            at: now,
            last_known: None,
        });
        c.at = now;
        (c, true)
    }

    /// Record a finished measurement. PURE.
    pub(crate) fn record(
        cache: Option<Self>,
        v: Option<Vec<Gpu>>,
        now: std::time::Instant,
    ) -> Self {
        let last_known = match &v {
            Some(g) => Some((g.clone(), now)),
            None => cache.and_then(|c| c.last_known),
        };
        GpuCache {
            reading: v,
            at: now,
            last_known,
        }
    }

    /// The reading the capability tokens use: the current one, or while it
    /// is UNKNOWN the last known one if younger than [`LAST_KNOWN_TTL`] —
    /// and nothing at all while `wedged`. PURE.
    pub(crate) fn for_capabilities(
        &self,
        now: std::time::Instant,
        wedged: bool,
    ) -> Option<Vec<Gpu>> {
        if wedged {
            return None;
        }
        self.reading.clone().or_else(|| {
            self.last_known
                .as_ref()
                .filter(|(_, at)| now.saturating_duration_since(*at) < LAST_KNOWN_TTL)
                .map(|(g, _)| g.clone())
        })
    }
}

static HOST_READING: std::sync::Mutex<Option<GpuCache>> = std::sync::Mutex::new(None);

/// Refresh the shared reading if due (single-flight), then hand back a copy.
/// Blocking on a miss for the caller that starts the measurement only.
fn refreshed() -> Option<GpuCache> {
    let now = std::time::Instant::now();
    {
        let Ok(mut g) = HOST_READING.lock() else {
            return None;
        };
        let (state, measure) = GpuCache::claim(g.take(), now);
        *g = Some(state.clone());
        if !measure {
            return Some(state);
        }
    }
    let v = measure_host();
    let Ok(mut g) = HOST_READING.lock() else {
        return None;
    };
    let next = GpuCache::record(g.take(), v, std::time::Instant::now());
    *g = Some(next.clone());
    Some(next)
}

/// The host's `gpus` for the computer report (`None` = UNKNOWN), reused for
/// [`GPU_READING_TTL`] — the reporter (every 300 s) and the 30 s device
/// heartbeat share one reading.
pub(crate) fn host_gpus_cached() -> Option<Vec<Gpu>> {
    refreshed().and_then(|c| c.reading)
}

/// The reading behind the heartbeat's `gpu:*` capability tokens: the current
/// reading, or the last known one while the current is UNKNOWN (see
/// [`LAST_KNOWN_TTL`]), and nothing while an `nvidia-smi` is wedged.
pub(crate) fn host_gpus_for_capabilities() -> Option<Vec<Gpu>> {
    let c = refreshed()?;
    c.for_capabilities(std::time::Instant::now(), abandoned_still_alive())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn nv(model: &str, mib: u64, driver: &str, cc: Option<&str>) -> Gpu {
        Gpu {
            vendor: "nvidia".into(),
            model: Some(model.into()),
            vram_bytes: Some(mib * 1024 * 1024),
            driver: Some(driver.into()),
            compute_capability: cc.map(str::to_string),
        }
    }

    #[test]
    fn the_query_is_the_amendments_query() {
        assert_eq!(
            QUERY_ARGS,
            [
                "--query-gpu=name,memory.total,driver_version,compute_cap",
                "--format=csv,noheader,nounits"
            ]
        );
    }

    #[test]
    fn several_gpus_parse_with_mib_converted_to_bytes() {
        let out = "Example GPU Model A, 32607, 999.10, 12.0\r\n\
                   Example GPU Model B, 8188, 999.10, 8.6\n";
        assert_eq!(
            parse_nvidia_smi(out),
            NvidiaReading::Gpus(vec![
                nv("Example GPU Model A", 32607, "999.10", Some("12.0")),
                nv("Example GPU Model B", 8188, "999.10", Some("8.6")),
            ])
        );
    }

    #[test]
    fn empty_output_is_zero_gpus_not_a_failure() {
        assert_eq!(parse_nvidia_smi(""), NvidiaReading::Gpus(vec![]));
        assert_eq!(parse_nvidia_smi("\n\r\n  \n"), NvidiaReading::Gpus(vec![]));
    }

    #[test]
    fn any_malformed_row_fails_the_whole_reading() {
        for bad in [
            // too few / too many fields
            "Example GPU, 8188, 999.10\n",
            "Example GPU, 8188, 999.10, 8.6, extra\n",
            // a non-number where MiB belongs
            "Example GPU, lots, 999.10, 8.6\n",
            // a negative / overflowing size
            "Example GPU, -1, 999.10, 8.6\n",
            "Example GPU, 99999999999999999999, 999.10, 8.6\n",
            // a capability that is not major.minor
            "Example GPU, 8188, 999.10, eight\n",
            "Example GPU, 8188, 999.10, 8.\n",
            // no name
            "[N/A], 8188, 999.10, 8.6\n",
            // the human-readable "no devices" text if it ever exits 0
            "No devices were found\n",
        ] {
            assert_eq!(parse_nvidia_smi(bad), NvidiaReading::Failed, "{bad:?}");
        }
        // One good row does not rescue a bad one.
        assert_eq!(
            parse_nvidia_smi("Example GPU, 8188, 999.10, 8.6\nbroken\n"),
            NvidiaReading::Failed
        );
    }

    #[test]
    fn an_na_compute_cap_is_null_not_a_failure() {
        let r = parse_nvidia_smi("Example GPU, [N/A], [N/A], [N/A]\n");
        assert_eq!(
            r,
            NvidiaReading::Gpus(vec![Gpu {
                vendor: "nvidia".into(),
                model: Some("Example GPU".into()),
                vram_bytes: None,
                driver: None,
                compute_capability: None,
            }])
        );
        // ...and such a GPU is not advertised as CUDA-capable.
        let NvidiaReading::Gpus(g) = r else {
            unreachable!()
        };
        assert!(capability_tokens(Some(&g)).is_empty());
    }

    #[test]
    fn an_overlong_model_is_null_and_too_many_gpus_is_unknown() {
        let long = "x".repeat(MAX_GPU_TEXT + 1);
        let NvidiaReading::Gpus(g) = parse_nvidia_smi(&format!("{long}, 1024, 1.0, 7.5\n")) else {
            panic!("an overlong name is a null model, not a malformed row");
        };
        assert_eq!(g[0].model, None);

        let many = "Example GPU, 1024, 1.0, 7.5\n".repeat(MAX_GPUS + 1);
        assert_eq!(parse_nvidia_smi(&many), NvidiaReading::Failed);
        let max = "Example GPU, 1024, 1.0, 7.5\n".repeat(MAX_GPUS);
        assert!(matches!(parse_nvidia_smi(&max), NvidiaReading::Gpus(g) if g.len() == MAX_GPUS));
    }

    #[test]
    fn the_largest_reading_fits_coords_json_bound() {
        // The worst case serde can make of a field `text()` accepts: every
        // byte a `"` or `\\`, each escaped to two bytes on the wire.
        let worst_text = "\"\\".repeat(MAX_GPU_TEXT / 2);
        assert_eq!(worst_text.len(), MAX_GPU_TEXT);
        let worst = Gpu {
            vendor: "nvidia".into(),
            model: Some(worst_text.clone()),
            vram_bytes: Some(u64::MAX),
            driver: Some(worst_text),
            compute_capability: Some("9".repeat(MAX_COMPUTE_CAP_LEN)),
        };
        let v = vec![worst; MAX_GPUS];
        let n = serde_json::to_vec(&v).unwrap().len();
        assert!(n < 11 * 1024, "{n} bytes");
        assert!(n < super::super::MAX_JSON_BYTES, "{n} bytes");
    }

    #[test]
    fn text_is_bounded_in_bytes_and_compute_cap_in_length() {
        // 32 four-byte characters = 128 bytes (accepted); 33 = 132 (null).
        let ok = "\u{1F600}".repeat(32);
        let over = "\u{1F600}".repeat(33);
        let NvidiaReading::Gpus(g) =
            parse_nvidia_smi(&format!("{ok}, 1, 1.0, 7.5\n{over}, 1, 1.0, 7.5\n"))
        else {
            panic!("long names are null models, not malformed rows");
        };
        assert_eq!(g[0].model.as_deref(), Some(ok.as_str()));
        assert_eq!(g[1].model, None);
        assert_eq!(
            parse_nvidia_smi("Example GPU, 1, 1.0, 1234567.0\n"),
            NvidiaReading::Failed
        );
    }

    #[test]
    fn an_absurd_vram_does_not_overflow_the_capability_token() {
        let mut g = nv("Example GPU", 1, "1.0", Some("8.6"));
        g.vram_bytes = Some(u64::MAX);
        let t = capability_tokens(Some(&[g]));
        assert_eq!(t[0], "gpu:cuda");
        assert_eq!(t[1], format!("gpu:vram:{}", u64::MAX / GIB));
    }

    #[test]
    fn pci_class_and_vendor_decide_gpu_presence() {
        assert_eq!(pci_device_is_gpu("0x030000\n", "0x10de\n"), Some(true)); // VGA
        assert_eq!(pci_device_is_gpu("0x030200", "0x10de"), Some(true)); // 3D, no display
        assert_eq!(pci_device_is_gpu("0x038000", "0x1002"), Some(true));
        assert_eq!(pci_device_is_gpu("0x120000", "0x1002"), Some(true)); // accelerator
        assert_eq!(pci_device_is_gpu("0x030000", "0x1a03"), Some(false)); // BMC VGA
        assert_eq!(pci_device_is_gpu("0x020000", "0x8086"), Some(false)); // Intel NIC
                                                                          // Fails closed: an accelerator or 3D controller of ANY vendor, and a
                                                                          // display controller of a vendor not known to be framebuffer-only.
        assert_eq!(pci_device_is_gpu("0x120000", "0x1da3"), Some(true));
        assert_eq!(pci_device_is_gpu("0x030200", "0x1a03"), Some(true));
        assert_eq!(pci_device_is_gpu("0x030000", "0x1ed5"), Some(true));
        assert_eq!(pci_device_is_gpu("garbage", "0x10de"), None);
        assert_eq!(pci_device_is_gpu("0x030000", ""), None);
    }

    /// A synthetic `/` for [`absence_evidence_at`].
    fn tree(
        pci: &[(&str, &str, &str)],
        drm: &[(&str, Option<&str>)],
        dev: &[&str],
    ) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        std::fs::create_dir_all(r.join("dev")).unwrap();
        for d in dev {
            std::fs::write(r.join("dev").join(d), "").unwrap();
        }
        std::fs::create_dir_all(r.join("proc/driver")).unwrap();
        std::fs::create_dir_all(r.join("sys/bus/pci/devices")).unwrap();
        for (addr, class, vendor) in pci {
            let d = r.join("sys/bus/pci/devices").join(addr);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("class"), class).unwrap();
            std::fs::write(d.join("vendor"), vendor).unwrap();
        }
        std::fs::create_dir_all(r.join("sys/class/drm")).unwrap();
        for (card, vendor) in drm {
            let d = r.join("sys/class/drm").join(card).join("device");
            std::fs::create_dir_all(&d).unwrap();
            if let Some(v) = vendor {
                std::fs::write(d.join("vendor"), v).unwrap();
            }
        }
        t
    }

    #[test]
    fn absence_is_proven_only_by_a_clean_tree() {
        // A headless server: a NIC and a BMC display adapter.
        let t = tree(
            &[
                ("0000:00:01.0", "0x020000", "0x8086"),
                ("0000:00:02.0", "0x030000", "0x1a03"),
            ],
            &[("card0", Some("0x1a03"))],
            &["null", "tty"],
        );
        let e = absence_evidence_at(t.path());
        assert!(e.proves_no_gpu(), "{e:?}");

        // A datacenter card with NO driver bound: no DRM card, no /dev node —
        // only PCI sees it, and it must block `[]`.
        let t = tree(&[("0000:41:00.0", "0x030200", "0x10de")], &[], &["null"]);
        let e = absence_evidence_at(t.path());
        assert_eq!(e.gpu_pci_device, Some(true));
        assert!(!e.proves_no_gpu());

        // A platform GPU (DRM card with no PCI vendor) is UNKNOWN.
        let t = tree(&[], &[("card0", None)], &["null"]);
        assert_eq!(absence_evidence_at(t.path()).gpu_class_drm_device, None);

        // The NVIDIA driver's nodes, and WSL's paravirtual device.
        let t = tree(&[], &[], &["nvidiactl", "dxg"]);
        let e = absence_evidence_at(t.path());
        assert_eq!(e.nvidia_driver_present, Some(true));
        assert_eq!(e.wsl_gpu_paravirt, Some(true));

        // The DRM name filter: connectors and render nodes (which have no
        // `device/vendor` of their own) are not cards and are skipped.
        let t = tree(
            &[("0000:00:02.0", "0x030000", "0x1a03")],
            &[
                ("card0", Some("0x1a03")),
                ("card0-HDMI-A-1", None),
                ("renderD128", None),
            ],
            &["null"],
        );
        assert_eq!(
            absence_evidence_at(t.path()).gpu_class_drm_device,
            Some(false)
        );
        // A GPU-vendor DRM card blocks `[]` on its own.
        let t = tree(&[], &[("card1", Some("0x8086"))], &["null"]);
        assert_eq!(
            absence_evidence_at(t.path()).gpu_class_drm_device,
            Some(true)
        );
        // `/proc/driver/nvidia` alone (no /dev nodes yet) is the driver.
        let t = tree(&[], &[], &["null"]);
        std::fs::create_dir_all(t.path().join("proc/driver/nvidia")).unwrap();
        assert_eq!(
            absence_evidence_at(t.path()).nvidia_driver_present,
            Some(true)
        );
        // A PCI entry with no readable class is UNKNOWN, not "not a GPU".
        let t = tree(&[], &[], &["null"]);
        std::fs::create_dir_all(t.path().join("sys/bus/pci/devices/0000:00:03.0")).unwrap();
        assert_eq!(absence_evidence_at(t.path()).gpu_pci_device, None);

        // No sysfs at all is UNKNOWN, never "no devices".
        let empty = tempfile::tempdir().unwrap();
        let e = absence_evidence_at(empty.path());
        assert_eq!(e.gpu_pci_device, None);
        assert_eq!(e.nvidia_driver_present, None);
        assert!(!e.proves_no_gpu());
    }

    /// A stub `nvidia-smi`, run as `sh -c <body>` — never by exec'ing a
    /// freshly written file, which a concurrent fork in this multi-threaded
    /// test binary can make fail with ETXTBSY.
    #[cfg(unix)]
    fn stub(body: &str) -> std::process::Command {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", body]);
        c
    }

    #[cfg(unix)]
    #[test]
    fn running_nvidia_smi_maps_every_outcome() {
        let d = tempfile::tempdir().unwrap();
        let t = Duration::from_secs(5);
        assert_eq!(
            run_nvidia_smi(&d.path().join("absent"), t),
            NvidiaReading::Absent
        );
        assert_eq!(
            run_command(stub("printf 'Example GPU, 8188, 999.10, 8.6\\n'"), t),
            NvidiaReading::Gpus(vec![nv("Example GPU", 8188, "999.10", Some("8.6"))])
        );
        // "No devices were found" exits non-zero.
        assert_eq!(
            run_command(stub("echo 'No devices were found'; exit 6"), t),
            NvidiaReading::Failed
        );
        // A wedged binary is killed at the timeout.
        let started = std::time::Instant::now();
        assert_eq!(
            run_command(stub("exec sleep 30"), Duration::from_millis(300)),
            NvidiaReading::Failed
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_wsl_fallback_is_tried_only_when_path_has_no_nvidia_smi() {
        let path = std::path::Path::new("nvidia-smi");
        let wsl = std::path::Path::new("/usr/lib/wsl/lib/nvidia-smi");
        let listed = NvidiaReading::Gpus(vec![nv("Example GPU", 1, "1.0", Some("8.6"))]);
        let tried = std::cell::RefCell::new(Vec::new());
        let run = |answers: Vec<(&'static std::path::Path, NvidiaReading)>| {
            move |p: &std::path::Path| {
                answers
                    .iter()
                    .find(|(q, _)| *q == p)
                    .map(|(_, r)| r.clone())
                    .unwrap()
            }
        };
        // PATH absent, fallback installed → the fallback's answer.
        let r = nvidia_reading_with(
            &[path, wsl],
            |p| {
                tried.borrow_mut().push(p.to_path_buf());
                true
            },
            run(vec![(path, NvidiaReading::Absent), (wsl, listed.clone())]),
        );
        assert_eq!(r, listed);
        // PATH answered (even with a failure) → the fallback is not run.
        let r = nvidia_reading_with(
            &[path, wsl],
            |_| true,
            run(vec![(path, NvidiaReading::Failed)]),
        );
        assert_eq!(r, NvidiaReading::Failed);
        // Fallback not installed → Absent.
        let r = nvidia_reading_with(
            &[path, wsl],
            |_| false,
            run(vec![(path, NvidiaReading::Absent)]),
        );
        assert_eq!(r, NvidiaReading::Absent);
        // The bare PATH name is never pre-checked with `exists`.
        assert_eq!(*tried.borrow(), vec![wsl.to_path_buf()]);
    }

    #[test]
    fn absence_evidence_is_never_read_after_a_failure() {
        assert!(wants_absence_evidence(&NvidiaReading::Absent));
        assert!(wants_absence_evidence(&NvidiaReading::Gpus(vec![])));
        assert!(!wants_absence_evidence(&NvidiaReading::Failed));
        assert!(!wants_absence_evidence(&NvidiaReading::Gpus(vec![nv(
            "x",
            1,
            "1",
            Some("8.6")
        )])));
    }

    #[test]
    fn a_claim_lets_exactly_one_caller_measure() {
        let t0 = std::time::Instant::now();
        // Cold: the first caller measures; the claim is visible to the next.
        let (c, measure) = GpuCache::claim(None, t0);
        assert!(measure);
        assert_eq!(c.reading, None);
        let (c2, measure) = GpuCache::claim(Some(c.clone()), t0 + Duration::from_millis(1));
        assert!(
            !measure,
            "a second caller during the measurement does not fork"
        );
        assert_eq!(c2, c);

        // Warm and due: the claimant moves `at`, keeping the old reading and
        // last-known for everyone else meanwhile.
        let g = vec![nv("Example GPU", 8188, "1.0", Some("8.6"))];
        let warm = GpuCache::record(None, Some(g.clone()), t0);
        let t1 = t0 + GPU_READING_TTL;
        let (claimed, measure) = GpuCache::claim(Some(warm), t1);
        assert!(measure);
        assert_eq!(claimed.at, t1);
        assert_eq!(claimed.reading, Some(g.clone()));
        let (_, measure) = GpuCache::claim(Some(claimed.clone()), t1);
        assert!(!measure);
        // ...and recording a failure over the claim keeps last-known.
        let failed = GpuCache::record(Some(claimed), None, t1);
        assert_eq!(failed.for_capabilities(t1, false), Some(g));
    }

    #[test]
    fn the_cache_is_single_flight_and_capabilities_outlive_one_failure() {
        let t0 = std::time::Instant::now();
        assert!(GpuCache::due(None, t0));
        let g = vec![nv("Example GPU", 8188, "1.0", Some("8.6"))];
        let c = GpuCache::record(None, Some(g.clone()), t0);
        assert!(!GpuCache::due(Some(&c), t0 + Duration::from_secs(10)));
        assert!(GpuCache::due(Some(&c), t0 + GPU_READING_TTL));

        // A failed measurement: the report says UNKNOWN, the capabilities
        // keep the last known reading for LAST_KNOWN_TTL, then drop it.
        let t1 = t0 + GPU_READING_TTL;
        let failed = GpuCache::record(Some(c), None, t1);
        assert_eq!(failed.reading, None);
        assert_eq!(failed.for_capabilities(t1, false), Some(g.clone()));
        assert_eq!(failed.for_capabilities(t0 + LAST_KNOWN_TTL, false), None);
        // A wedged `nvidia-smi` withdraws the tokens at once — even with a
        // current reading.
        assert_eq!(failed.for_capabilities(t1, true), None);
        let current = GpuCache::record(None, Some(g.clone()), t1);
        assert_eq!(current.for_capabilities(t1, true), None);

        // A measured `[]` is known, and replaces the last known list.
        let none = GpuCache::record(Some(failed), Some(vec![]), t1);
        assert_eq!(none.for_capabilities(t1, false), Some(vec![]));
    }

    fn proven() -> AbsenceEvidence {
        AbsenceEvidence {
            nvidia_driver_present: Some(false),
            wsl_gpu_paravirt: Some(false),
            gpu_pci_device: Some(false),
            gpu_class_drm_device: Some(false),
        }
    }

    #[test]
    fn empty_list_needs_positive_proof_and_failure_is_always_unknown() {
        let g = vec![nv("Example GPU", 8188, "1.0", Some("8.6"))];
        assert_eq!(decide(NvidiaReading::Gpus(g.clone()), None), Some(g));

        assert_eq!(decide(NvidiaReading::Absent, Some(proven())), Some(vec![]));
        assert_eq!(
            decide(NvidiaReading::Gpus(vec![]), Some(proven())),
            Some(vec![])
        );

        // Off Linux there is no evidence: absent binary is UNKNOWN.
        assert_eq!(decide(NvidiaReading::Absent, None), None);
        // A failure is never `[]`, whatever the evidence says.
        assert_eq!(decide(NvidiaReading::Failed, Some(proven())), None);

        // Each single dissenting or unreadable check keeps it UNKNOWN.
        for e in [
            AbsenceEvidence {
                nvidia_driver_present: Some(true),
                ..proven()
            },
            AbsenceEvidence {
                nvidia_driver_present: None,
                ..proven()
            },
            AbsenceEvidence {
                wsl_gpu_paravirt: Some(true),
                ..proven()
            },
            AbsenceEvidence {
                wsl_gpu_paravirt: None,
                ..proven()
            },
            AbsenceEvidence {
                gpu_class_drm_device: Some(true),
                ..proven()
            },
            AbsenceEvidence {
                gpu_class_drm_device: None,
                ..proven()
            },
            AbsenceEvidence {
                gpu_pci_device: Some(true),
                ..proven()
            },
            AbsenceEvidence {
                gpu_pci_device: None,
                ..proven()
            },
        ] {
            assert_eq!(decide(NvidiaReading::Absent, Some(e)), None, "{e:?}");
        }
    }

    #[test]
    fn display_vendors_fail_closed_except_known_framebuffers() {
        assert_eq!(display_vendor_may_be_gpu("0x10de\n"), Some(true));
        assert_eq!(display_vendor_may_be_gpu("0x1002"), Some(true));
        assert_eq!(display_vendor_may_be_gpu("0x8086\n"), Some(true));
        // An unlisted vendor is a possible GPU, not a framebuffer.
        assert_eq!(display_vendor_may_be_gpu("0x1ed5"), Some(true));
        assert_eq!(display_vendor_may_be_gpu("0x1a03\n"), Some(false)); // BMC VGA
        assert_eq!(display_vendor_may_be_gpu("0x1af4\n"), Some(false)); // virtio
        assert_eq!(display_vendor_may_be_gpu(""), None);
        assert_eq!(display_vendor_may_be_gpu("garbage"), None);
    }

    #[test]
    fn a_guest_takes_the_hosts_reading_only_through_dxg() {
        let host = vec![nv("Example GPU", 8188, "1.0", Some("8.6"))];
        assert_eq!(guest_gpus(Some(true), Some(&host)), Some(host.clone()));
        assert_eq!(guest_gpus(Some(true), None), None);
        assert_eq!(guest_gpus(Some(false), Some(&host)), Some(vec![]));
        assert_eq!(guest_gpus(Some(false), None), Some(vec![]));
        assert_eq!(guest_gpus(None, Some(&host)), None);
    }

    #[test]
    fn capability_tokens_name_cuda_and_the_largest_cards_vram() {
        let g = vec![
            nv("Example GPU Model B", 8188, "1.0", Some("8.6")),
            nv("Example GPU Model A", 32607, "1.0", Some("12.0")),
        ];
        assert_eq!(capability_tokens(Some(&g)), vec!["gpu:cuda", "gpu:vram:32"]);
        assert_eq!(
            capability_tokens(Some(&[nv("Example GPU", 24564, "1.0", Some("8.9"))])),
            vec!["gpu:cuda", "gpu:vram:24"]
        );
        // VRAM unknown: CUDA still advertised, no VRAM token.
        let mut no_vram = nv("Example GPU", 1, "1.0", Some("8.6"));
        no_vram.vram_bytes = None;
        assert_eq!(capability_tokens(Some(&[no_vram])), vec!["gpu:cuda"]);
        // Unknown or none: nothing.
        assert!(capability_tokens(None).is_empty());
        assert!(capability_tokens(Some(&[])).is_empty());
        // A sub-half-GiB card rounds to zero and gets no VRAM token.
        assert_eq!(
            capability_tokens(Some(&[Gpu {
                vram_bytes: Some(GIB / 4),
                ..nv("x", 0, "1", Some("5.0"))
            }])),
            vec!["gpu:cuda"]
        );
    }

    #[test]
    fn the_wire_shape_is_the_amendments_shape() {
        let v = serde_json::to_value(vec![nv("Example GPU", 1024, "999.10", Some("8.6"))]).unwrap();
        assert_eq!(
            v,
            serde_json::json!([{
                "vendor": "nvidia",
                "model": "Example GPU",
                "vram_bytes": 1073741824_u64,
                "driver": "999.10",
                "compute_capability": "8.6"
            }])
        );
    }
}
