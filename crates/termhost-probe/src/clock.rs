//! The probe clock, in nanoseconds, on the timebase Node's `process.hrtime`
//! uses — QueryPerformanceCounter on Windows, `CLOCK_MONOTONIC` on Linux — so a
//! generator's `t_emit_ns` and a captured frame's time are directly comparable
//! (plan D2; the selftest's `clock.aligned` check measures it with a pipe round
//! trip rather than assuming it).
//!
//! WGC frame times (`SystemRelativeTime`, 100 ns units of QPC time) and DXGI
//! duplication times (`LastPresentTime`, raw QPC ticks) convert onto the same
//! timebase with [`hundred_ns_to_ns`] and [`qpc_ticks_to_ns`].

use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

    pub const NAME: &str = "qpc";

    pub fn frequency() -> u64 {
        static FREQ: OnceLock<u64> = OnceLock::new();
        *FREQ.get_or_init(|| {
            let mut f = 0i64;
            // SAFETY: QueryPerformanceFrequency writes one i64; it cannot fail on
            // Windows XP and later.
            unsafe { QueryPerformanceFrequency(&mut f) }.expect("QueryPerformanceFrequency");
            f.max(1) as u64
        })
    }

    /// Raw QPC ticks -> ns (the same scaling libuv's `uv_hrtime` applies).
    pub fn qpc_ticks_to_ns(ticks: i64) -> u64 {
        (u128::from(ticks.max(0) as u64) * 1_000_000_000 / u128::from(frequency())) as u64
    }

    pub fn now_ns() -> u64 {
        let mut c = 0i64;
        // SAFETY: QueryPerformanceCounter writes one i64; it cannot fail on
        // Windows XP and later.
        unsafe { QueryPerformanceCounter(&mut c) }.expect("QueryPerformanceCounter");
        qpc_ticks_to_ns(c)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    pub const NAME: &str = "clock_monotonic";

    pub fn now_ns() -> u64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock_gettime writes one timespec; CLOCK_MONOTONIC always exists
        // on Linux.
        let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        assert_eq!(rc, 0, "clock_gettime(CLOCK_MONOTONIC)");
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use std::sync::OnceLock;
    use std::time::Instant;

    /// Not Node's timebase: generator times cannot be compared on this platform,
    /// and `clock.aligned` says so.
    pub const NAME: &str = "process_instant";

    pub fn now_ns() -> u64 {
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64
    }
}

pub use imp::*;

/// WGC `SystemRelativeTime` (a TimeSpan of 100 ns units of QPC time) -> ns.
pub fn hundred_ns_to_ns(duration: i64) -> u64 {
    duration.max(0) as u64 * 100
}

/// The name of this platform's clock, recorded in every `start` record.
pub fn clock_name() -> &'static str {
    NAME
}

/// Nanoseconds -> milliseconds.
pub fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

/// A signed difference in milliseconds (`a - b`).
pub fn delta_ms(a: u64, b: u64) -> f64 {
    (a as i128 - b as i128) as f64 / 1e6
}

/// Sleep until the probe clock reads `t_ns`: a coarse sleep to 2 ms before,
/// then a spin — scheduled paints and keys land within a fraction of a
/// millisecond of their schedule.
pub fn sleep_until_ns(t_ns: u64) {
    loop {
        let now = now_ns();
        if now >= t_ns {
            return;
        }
        let left = t_ns - now;
        if left > 2_000_000 {
            std::thread::sleep(Duration::from_nanos(left - 2_000_000));
        } else {
            std::hint::spin_loop();
        }
    }
}

/// UTC wall-clock time as RFC 3339 with milliseconds (the results schema's
/// `dateTime`).
pub fn wall_rfc3339() -> String {
    rfc3339(SystemTime::now())
}

/// Format a `SystemTime` as RFC 3339 UTC.
pub fn rfc3339(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let millis = d.subsec_millis();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, day) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 -> (year, month, day) — Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_is_monotonic_and_nanosecond_scaled() {
        let a = now_ns();
        std::thread::sleep(Duration::from_millis(20));
        let b = now_ns();
        let elapsed = b - a;
        assert!((15_000_000..2_000_000_000).contains(&elapsed), "{elapsed}");
    }

    #[test]
    fn sleep_until_lands_on_time() {
        let target = now_ns() + 30_000_000;
        sleep_until_ns(target);
        let late = now_ns() - target;
        assert!(late < 5_000_000, "late by {late} ns");
    }

    #[test]
    fn rfc3339_formats_known_instants() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        let t = UNIX_EPOCH + Duration::from_millis(1_760_000_000_123);
        assert_eq!(rfc3339(t), "2025-10-09T08:53:20.123Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(rfc3339(leap), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn deltas_are_signed() {
        assert_eq!(delta_ms(5_000_000, 2_000_000), 3.0);
        assert_eq!(delta_ms(2_000_000, 5_000_000), -3.0);
        assert_eq!(hundred_ns_to_ns(15), 1500);
    }
}
