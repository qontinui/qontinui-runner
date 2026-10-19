//! Per-process accounting on Windows: `GetProcessTimes` (kernel + user CPU),
//! `GetProcessMemoryInfo` (`PrivateUsage` — the catalog's memory number — and
//! the working set), and PDH `\GPU Engine(*)\Utilization Percentage`, with the
//! instances enumerated on the machine rather than assumed.

use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_NO_OBJECT, PDH_CSTATUS_VALID_DATA,
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

use crate::platform::common::fold_gpu;
use crate::platform::{GpuReading, ProcMem, ProcessReader};

/// A process handle closed on drop.
struct Proc(HANDLE);

impl Proc {
    fn open(pid: u32, vm_read: bool) -> Option<Proc> {
        let access = if vm_read {
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ
        } else {
            PROCESS_QUERY_LIMITED_INFORMATION
        };
        // SAFETY: opening a handle we close in Drop.
        unsafe { OpenProcess(access, false, pid) }.ok().map(Proc)
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        // SAFETY: our own handle, closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn filetime_ns(ft: FILETIME) -> u64 {
    ((u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)) * 100
}

/// The GPU engine counter query.
pub struct Pdh {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
}

// SAFETY: PDH handles are process-wide and the query is used by one thread
// at a time (the sampler's owner).
unsafe impl Send for Pdh {}

impl Pdh {
    pub fn open() -> Result<Pdh, String> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: out-pointer to a local; closed in Drop (or below on error).
        unsafe {
            let st = PdhOpenQueryW(windows::core::PCWSTR::null(), 0, &mut query);
            if st != 0 {
                return Err(format!("PdhOpenQueryW: 0x{st:08x}"));
            }
            let mut counter = PDH_HCOUNTER::default();
            let st = PdhAddEnglishCounterW(
                query,
                w!("\\GPU Engine(*)\\Utilization Percentage"),
                0,
                &mut counter,
            );
            if st != 0 {
                let _ = PdhCloseQuery(query);
                return Err(if st == PDH_CSTATUS_NO_OBJECT {
                    "the GPU Engine counter set does not exist here (no WDDM 2.0+ GPU, e.g. a GPU-less VM)".to_string()
                } else {
                    format!("PdhAddEnglishCounterW(\\GPU Engine(*)\\Utilization Percentage): 0x{st:08x}")
                });
            }
            // A rate counter needs a first sample to difference against.
            let _ = PdhCollectQueryData(query);
            Ok(Pdh { query, counter })
        }
    }

    /// (instance name, utilisation %) for every instance.
    pub fn collect(&mut self) -> Result<Vec<(String, f64)>, String> {
        // SAFETY: the buffer is sized by PDH's own first call and is 8-byte
        // aligned; names point into it and are read before it is dropped.
        unsafe {
            let st = PdhCollectQueryData(self.query);
            if st != 0 {
                return Err(format!("PdhCollectQueryData: 0x{st:08x}"));
            }
            let mut bytes = 0u32;
            let mut count = 0u32;
            let st = PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut bytes,
                &mut count,
                None,
            );
            if st != PDH_MORE_DATA {
                if st == 0 {
                    return Ok(Vec::new());
                }
                return Err(format!("PdhGetFormattedCounterArrayW (size): 0x{st:08x}"));
            }
            let mut buf = vec![0u64; (bytes as usize).div_ceil(8)];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            let st = PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut bytes,
                &mut count,
                Some(items),
            );
            if st != 0 {
                return Err(format!("PdhGetFormattedCounterArrayW: 0x{st:08x}"));
            }
            let mut out = Vec::with_capacity(count as usize);
            for i in 0..count as usize {
                let item = &*items.add(i);
                let status = item.FmtValue.CStatus;
                if status != PDH_CSTATUS_VALID_DATA && status != PDH_CSTATUS_NEW_DATA {
                    continue;
                }
                let name = item.szName.to_string().unwrap_or_default();
                out.push((name, item.FmtValue.Anonymous.doubleValue));
            }
            Ok(out)
        }
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        // SAFETY: our own query, closed once.
        let _ = unsafe { PdhCloseQuery(self.query) };
    }
}

pub struct WinProcessReader {
    pdh: Result<Pdh, String>,
}

impl WinProcessReader {
    pub fn new() -> WinProcessReader {
        WinProcessReader { pdh: Pdh::open() }
    }
}

impl ProcessReader for WinProcessReader {
    fn cpu_ns(&mut self, pid: u32) -> Option<u64> {
        let p = Proc::open(pid, false)?;
        let (mut c, mut e, mut k, mut u) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: four out-pointers to locals.
        unsafe { GetProcessTimes(p.0, &mut c, &mut e, &mut k, &mut u) }.ok()?;
        Some(filetime_ns(k) + filetime_ns(u))
    }

    fn memory(&mut self, pid: u32) -> Option<ProcMem> {
        let p = Proc::open(pid, true).or_else(|| Proc::open(pid, false))?;
        let mut m = PROCESS_MEMORY_COUNTERS_EX {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        // SAFETY: the EX struct starts with the base struct; cb says its size.
        unsafe {
            GetProcessMemoryInfo(
                p.0,
                &mut m as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            )
        }
        .ok()?;
        Some(ProcMem {
            catalog_bytes: m.PrivateUsage as u64,
            rss_bytes: m.WorkingSetSize as u64,
        })
    }

    fn gpu(&mut self) -> GpuReading {
        match self.pdh.as_mut() {
            Err(why) => GpuReading::Unsupported(why.clone()),
            Ok(pdh) => match pdh.collect() {
                Ok(items) => GpuReading::Measured(fold_gpu(items)),
                Err(why) => GpuReading::Failed(why),
            },
        }
    }
}

/// The number of GPU engine instances (the doctor's `counters.gpu`).
pub fn gpu_instances() -> Result<usize, String> {
    let mut pdh = Pdh::open()?;
    std::thread::sleep(std::time::Duration::from_millis(120));
    pdh.collect().map(|v| v.len())
}
