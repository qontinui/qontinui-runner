//! The WMI route of a holder spawn (plan D4, built in Phase 3): `Win32_Process.Create`
//! through COM, for a spawner inside a job that refuses breakaway.
//!
//! The process is created by the WMI provider host (`WmiPrvSE.exe`) on our
//! behalf, under our own token (a local connection impersonates the caller),
//! so it is in NONE of our jobs — the property the route exists for — and it
//! is NOT our child: there is no `std::process::Child`, no inherited pipe, and
//! no parent to reap it (Windows has no zombies; a process handle is all there
//! is). [`WmiProcess`] is that handle.
//!
//! Identity: WMI returns a pid, and a pid is a name. The handle is opened
//! immediately; the image behind it must be the executable we asked for
//! (`QueryFullProcessImageNameW`) AND the process must have been created
//! while `Create` ran (`GetProcessTimes`), else the pid was reused in between
//! — possibly by another pane's holder, same image — and is refused WITHOUT
//! being signalled ([`HolderSpawnError::WmiNotOurProcess`]).
//! From then on the handle pins the process: its pid cannot be reused while
//! the handle is open.
//!
//! The COM work runs on a short-lived thread of its own, initialised as a
//! multithreaded apartment there and uninitialised before it ends, so the
//! caller's thread keeps whatever COM state it had (a Tauri main thread is
//! single-threaded-apartment).

use std::ffi::OsString;
use std::mem::ManuallyDrop;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use windows::core::{IUnknown, BSTR, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Variant::{VariantClear, VARIANT, VT_BSTR, VT_I4, VT_UNKNOWN};
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator, WBEM_FLAG_RETURN_WBEM_COMPLETE,
};
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
    WaitForSingleObject, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};

use super::{quote_windows_arg, HolderSpawnError, HolderSpawnRoute};

/// `Win32_ProcessStartup.ShowWindow`: `SW_HIDE`.
const SW_HIDE: i32 = 0;

/// `GetExitCodeProcess`'s answer while the process runs.
const STILL_ACTIVE: u32 = 259;

/// A holder spawned through WMI: an owned process handle, opened right after
/// `Win32_Process.Create` returned and verified to be the holder executable.
#[derive(Debug)]
pub struct WmiProcess {
    pid: u32,
    handle: HANDLE,
}

// SAFETY: a process handle is a kernel object handle, usable from any thread;
// every operation on it here is a thread-safe Win32 call.
unsafe impl Send for WmiProcess {}
// SAFETY: as above.
unsafe impl Sync for WmiProcess {}

impl WmiProcess {
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// The exit code once the process has exited; `None` while it runs.
    pub fn try_wait(&self) -> std::io::Result<Option<u32>> {
        self.wait_timeout(Duration::ZERO)
    }

    /// Wait at most `timeout` for the process to exit.
    pub fn wait_timeout(&self, timeout: Duration) -> std::io::Result<Option<u32>> {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: an owned handle opened with PROCESS_SYNCHRONIZE.
        match unsafe { WaitForSingleObject(self.handle, ms) } {
            WAIT_OBJECT_0 => {
                let mut code: u32 = STILL_ACTIVE;
                // SAFETY: an owned handle with query access; `code` is a valid
                // out-pointer.
                if unsafe { GetExitCodeProcess(self.handle, &mut code) } == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(Some(code))
            }
            WAIT_TIMEOUT => Ok(None),
            _ => Err(std::io::Error::last_os_error()),
        }
    }

    /// End the process through its handle — never by pid.
    pub fn kill(&self) -> std::io::Result<()> {
        // SAFETY: an owned handle opened with PROCESS_TERMINATE.
        if unsafe { TerminateProcess(self.handle, 1) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for WmiProcess {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and closed exactly once. Closing it does
        // not end the process.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Spawn `cmd`'s program, arguments and working directory through WMI
/// `Win32_Process.Create`, hidden and detached
/// ([`HolderSpawnRoute::Wmi`]'s startup flags). `cmd`'s environment is NOT
/// carried: the process gets the user's default environment (see the parent
/// module's docs). The caller tells the holder to write its ready line to a
/// file, since there is no pipe to print it on.
///
/// Bounded by `timeout`: `Win32_Process.Create` is synchronous and a wedged
/// WMI service would otherwise hold the new-tab path forever. Past it the COM
/// thread is left to finish on its own; a holder it still creates finds its
/// spec already removed by the caller and exits `holder_error=spec`.
pub fn spawn_holder_wmi(cmd: &Command, timeout: Duration) -> Result<WmiProcess, HolderSpawnError> {
    let program = PathBuf::from(cmd.get_program());
    let mut line = quote_windows_arg(program.to_str().ok_or_else(|| {
        HolderSpawnError::Wmi(format!("program {} is not Unicode", program.display()))
    })?);
    for arg in cmd.get_args() {
        let arg = arg
            .to_str()
            .ok_or_else(|| HolderSpawnError::Wmi(format!("argument {arg:?} is not Unicode")))?;
        line.push(' ');
        line.push_str(&quote_windows_arg(arg));
    }
    let cwd = cmd.get_current_dir().map(Path::to_path_buf);
    let called = filetime_now().saturating_sub(CLOCK_SLACK_BEFORE);
    // A fresh thread: COM is initialised as MTA there and uninitialised before
    // it ends, whatever apartment the caller's thread is in.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("pty-holder-wmi".into())
        .spawn(move || {
            let _ = tx.send(create_on_this_thread(&line, cwd.as_deref()));
        })
        .map_err(|e| HolderSpawnError::Wmi(format!("COM thread: {e}")))?;
    let pid = match rx.recv_timeout(timeout) {
        Ok(result) => result?,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            return Err(HolderSpawnError::Wmi(format!(
                "no answer from Win32_Process.Create within {} ms",
                timeout.as_millis()
            )))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            return Err(HolderSpawnError::Wmi("COM thread panicked".into()))
        }
    };
    let returned = filetime_now() + CLOCK_SLACK_AFTER;
    open_verified(pid, &program, called..=returned)
}

/// FILETIME units (100 ns) between 1601-01-01 and the Unix epoch.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

/// Slack on the creation-time window, in FILETIME units: the process clock
/// and `SystemTime` are one clock, but read at different instants.
const CLOCK_SLACK_BEFORE: u64 = 10_000_000; // 1 s
const CLOCK_SLACK_AFTER: u64 = 1_000_000; // 100 ms

/// Now, as a FILETIME value (100 ns since 1601-01-01) — the unit process
/// creation times are in.
pub fn filetime_now() -> u64 {
    let since_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() / 100)
        .unwrap_or(0);
    FILETIME_UNIX_EPOCH + u64::try_from(since_unix).unwrap_or(0)
}

/// Open `pid` and require that its image is `program` AND that it was created
/// inside `created_within` — between the `Create` call and its return. The
/// holder WMI made was; a process that took over its pid after it exited was
/// created later, so a same-image impostor (another pane's holder) is refused
/// too, without being signalled.
fn open_verified(
    pid: u32,
    program: &Path,
    created_within: std::ops::RangeInclusive<u64>,
) -> Result<WmiProcess, HolderSpawnError> {
    // SAFETY: no pointers; a non-null result is owned by the WmiProcess (or
    // closed below).
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return Err(HolderSpawnError::WmiNotOurProcess {
            pid,
            detail: format!("OpenProcess: {}", std::io::Error::last_os_error()),
        });
    }
    let process = WmiProcess { pid, handle };
    let image = image_path(handle).map_err(|e| HolderSpawnError::WmiNotOurProcess {
        pid,
        detail: format!("QueryFullProcessImageNameW: {e}"),
    })?;
    if !same_file(&image, program) {
        // `process` drops here: its handle is closed, nothing is signalled.
        return Err(HolderSpawnError::WmiNotOurProcess {
            pid,
            detail: format!("its image is {}", image.display()),
        });
    }
    // SAFETY: `handle` was opened with PROCESS_QUERY_LIMITED_INFORMATION.
    match unsafe { creation_time(handle) } {
        Some(created) if created_within.contains(&created) => Ok(process),
        created => Err(HolderSpawnError::WmiNotOurProcess {
            pid,
            detail: format!(
                "created at FILETIME {created:?}, outside the Create call's window \
                 {created_within:?}"
            ),
        }),
    }
}

/// The creation time of the process behind `handle`, as one FILETIME value.
///
/// # Safety
/// `handle` must be a valid process handle with at least
/// `PROCESS_QUERY_LIMITED_INFORMATION` access.
unsafe fn creation_time(handle: HANDLE) -> Option<u64> {
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: the caller's guarantee; the out-pointers are valid locals.
    let ok = unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) };
    (ok != 0).then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

fn image_path(handle: HANDLE) -> std::io::Result<PathBuf> {
    let mut buf = vec![0u16; 32 * 1024];
    let mut len = buf.len() as u32;
    // SAFETY: a handle with PROCESS_QUERY_LIMITED_INFORMATION; `buf` holds
    // `len` units and `len` is updated to the written length.
    if unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) }
        == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    buf.truncate(len as usize);
    Ok(PathBuf::from(OsString::from_wide(&buf)))
}

/// The same file, as Windows names it: canonicalised where possible, compared
/// without ASCII case.
fn same_file(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let wide = |p: PathBuf| -> Vec<u16> {
        p.as_os_str()
            .encode_wide()
            .map(|u| {
                if (u16::from(b'A')..=u16::from(b'Z')).contains(&u) {
                    u + 32
                } else {
                    u
                }
            })
            .collect()
    };
    wide(canon(a)) == wide(canon(b))
}

/// An owned VARIANT, cleared on drop.
struct Variant(VARIANT);

impl Variant {
    fn empty() -> Self {
        Variant(VARIANT::default())
    }

    fn bstr(text: &str) -> Self {
        let mut v = VARIANT::default();
        // SAFETY: writing the discriminant and the matching union member of a
        // freshly zeroed VARIANT; ownership of the BSTR moves into it and is
        // released by VariantClear in Drop.
        unsafe {
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = VT_BSTR;
            inner.Anonymous.bstrVal = ManuallyDrop::new(BSTR::from(text));
        }
        Variant(v)
    }

    fn i4(n: i32) -> Self {
        let mut v = VARIANT::default();
        // SAFETY: as above, for a plain integer member.
        unsafe {
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = VT_I4;
            inner.Anonymous.lVal = n;
        }
        Variant(v)
    }

    fn object(obj: &IWbemClassObject) -> Self {
        // One AddRef, owned by the VARIANT and released by VariantClear.
        let unknown: IUnknown = obj.clone().into();
        let mut v = VARIANT::default();
        // SAFETY: as above, for the interface member.
        unsafe {
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = VT_UNKNOWN;
            inner.Anonymous.punkVal = ManuallyDrop::new(Some(unknown));
        }
        Variant(v)
    }

    /// The value as an integer, for the `VT_I4` (CIM `uint32`) outputs of
    /// `Create`.
    fn as_i32(&self) -> Option<i32> {
        // SAFETY: the discriminant is read first and the member read only when
        // it says that member is live.
        unsafe {
            let inner = &*self.0.Anonymous.Anonymous;
            (inner.vt == VT_I4).then_some(inner.Anonymous.lVal)
        }
    }
}

impl Drop for Variant {
    fn drop(&mut self) {
        // SAFETY: a VARIANT this type initialised (or WMI filled); cleared once.
        unsafe {
            let _ = VariantClear(&mut self.0);
        }
    }
}

fn wmi_err(what: &str) -> impl Fn(windows::core::Error) -> HolderSpawnError + '_ {
    move |e| HolderSpawnError::Wmi(format!("{what}: {e}"))
}

/// Initialise COM (MTA) for this thread, run `Win32_Process.Create`, and
/// uninitialise. Returns the new process's pid.
fn create_on_this_thread(command_line: &str, cwd: Option<&Path>) -> Result<u32, HolderSpawnError> {
    // SAFETY: COM initialisation for this thread only, balanced below when it
    // succeeded (S_OK or S_FALSE).
    let init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if init.is_err() {
        return Err(HolderSpawnError::Wmi(format!("CoInitializeEx: {init:?}")));
    }
    let result = create(command_line, cwd);
    // SAFETY: balances the successful CoInitializeEx above; every COM object
    // `create` made has been released by the time it returns.
    unsafe { CoUninitialize() };
    result
}

fn create(command_line: &str, cwd: Option<&Path>) -> Result<u32, HolderSpawnError> {
    // SAFETY: plain COM calls on interfaces this function owns; every
    // out-pointer is a valid local; every VARIANT is owned by a `Variant`.
    unsafe {
        let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)
            .map_err(wmi_err("CoCreateInstance(WbemLocator)"))?;
        let services: IWbemServices = locator
            .ConnectServer(
                &BSTR::from("ROOT\\CIMV2"),
                &BSTR::new(),
                &BSTR::new(),
                &BSTR::new(),
                0,
                &BSTR::new(),
                None,
            )
            .map_err(wmi_err("ConnectServer(ROOT\\CIMV2)"))?;
        CoSetProxyBlanket(
            &services,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )
        .map_err(wmi_err("CoSetProxyBlanket"))?;

        let class = get_class(&services, "Win32_Process")?;
        let mut in_sig: Option<IWbemClassObject> = None;
        class
            .GetMethod(&BSTR::from("Create"), 0, &mut in_sig, std::ptr::null_mut())
            .map_err(wmi_err("GetMethod(Create)"))?;
        let in_params = in_sig
            .ok_or_else(|| HolderSpawnError::Wmi("Create has no input signature".into()))?
            .SpawnInstance(0)
            .map_err(wmi_err("SpawnInstance(Create params)"))?;

        let startup = get_class(&services, "Win32_ProcessStartup")?
            .SpawnInstance(0)
            .map_err(wmi_err("SpawnInstance(Win32_ProcessStartup)"))?;
        put(
            &startup,
            "CreateFlags",
            &Variant::i4(HolderSpawnRoute::Wmi.creation_flags() as i32),
        )?;
        put(&startup, "ShowWindow", &Variant::i4(SW_HIDE))?;

        put(&in_params, "CommandLine", &Variant::bstr(command_line))?;
        if let Some(cwd) = cwd {
            let cwd = cwd
                .to_str()
                .ok_or_else(|| HolderSpawnError::Wmi("working directory is not Unicode".into()))?;
            put(&in_params, "CurrentDirectory", &Variant::bstr(cwd))?;
        }
        put(
            &in_params,
            "ProcessStartupInformation",
            &Variant::object(&startup),
        )?;

        let mut out: Option<IWbemClassObject> = None;
        services
            .ExecMethod(
                &BSTR::from("Win32_Process"),
                &BSTR::from("Create"),
                WBEM_FLAG_RETURN_WBEM_COMPLETE,
                None,
                &in_params,
                Some(&mut out),
                None,
            )
            .map_err(wmi_err("ExecMethod(Win32_Process.Create)"))?;
        let out = out.ok_or_else(|| HolderSpawnError::Wmi("Create returned no output".into()))?;
        let rv = get(&out, "ReturnValue")?
            .as_i32()
            .ok_or_else(|| HolderSpawnError::Wmi("ReturnValue is not an integer".into()))?;
        if rv != 0 {
            return Err(HolderSpawnError::Wmi(format!("ReturnValue {rv}")));
        }
        let pid = get(&out, "ProcessId")?
            .as_i32()
            .ok_or_else(|| HolderSpawnError::Wmi("ProcessId is not an integer".into()))?;
        u32::try_from(pid)
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| HolderSpawnError::Wmi(format!("ProcessId {pid} is not a pid")))
    }
}

/// # Safety
/// `services` must be a live, proxy-blanketed `IWbemServices`.
unsafe fn get_class(
    services: &IWbemServices,
    name: &str,
) -> Result<IWbemClassObject, HolderSpawnError> {
    let mut class: Option<IWbemClassObject> = None;
    // SAFETY: the caller's guarantee; `class` is a valid out-pointer.
    unsafe {
        services
            .GetObject(
                &BSTR::from(name),
                WBEM_FLAG_RETURN_WBEM_COMPLETE,
                None,
                Some(&mut class),
                None,
            )
            .map_err(|e| HolderSpawnError::Wmi(format!("GetObject({name}): {e}")))?;
    }
    class.ok_or_else(|| HolderSpawnError::Wmi(format!("GetObject({name}) returned nothing")))
}

/// # Safety
/// `obj` must be a live `IWbemClassObject`.
unsafe fn put(obj: &IWbemClassObject, name: &str, value: &Variant) -> Result<(), HolderSpawnError> {
    // SAFETY: the caller's guarantee; `value` outlives the call, and Put
    // copies it.
    unsafe {
        obj.Put(&BSTR::from(name), 0, &value.0, 0)
            .map_err(|e| HolderSpawnError::Wmi(format!("Put({name}): {e}")))
    }
}

/// # Safety
/// `obj` must be a live `IWbemClassObject`.
unsafe fn get(obj: &IWbemClassObject, name: &str) -> Result<Variant, HolderSpawnError> {
    let mut value = Variant::empty();
    // SAFETY: the caller's guarantee; `value.0` is a valid, empty VARIANT that
    // Get fills and `Variant` clears.
    unsafe {
        obj.Get(&BSTR::from(name), 0, &mut value.0, None, None)
            .map_err(|e| HolderSpawnError::Wmi(format!("Get({name}): {e}")))?;
    }
    Ok(value)
}
