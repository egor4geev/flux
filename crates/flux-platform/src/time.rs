//! The local time, by the system's rules.

/// How far local time is ahead of UTC at `at` (seconds since 1970), in seconds.
pub fn local_utc_offset(at: i64) -> i32 {
    #[cfg(unix)]
    {
        let time = at as libc::time_t;
        // SAFETY: `localtime_r` writes only into `tm`, which outlives the call.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
            return 0;
        }
        tm.tm_gmtoff as i32
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows_sys::Win32::System::Time::{
            FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime,
        };
        /// 100-nanosecond ticks from 1601 (where Windows counts from) to 1970.
        const UNIX_EPOCH: i64 = 116_444_736_000_000_000;
        let Some(ticks) = at
            .checked_mul(10_000_000)
            .and_then(|ticks| ticks.checked_add(UNIX_EPOCH))
            .filter(|&ticks| ticks >= 0)
        else {
            return 0;
        };
        let utc_file = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        // SAFETY: plain C structs, written by the calls before they are read.
        let (mut utc, mut local): (SYSTEMTIME, SYSTEMTIME) = unsafe { std::mem::zeroed() };
        let mut local_file: FILETIME = unsafe { std::mem::zeroed() };
        // SAFETY: every pointer is to a live local; a null zone is the system's current one.
        let done = unsafe {
            FileTimeToSystemTime(&utc_file, &mut utc) != 0
                && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) != 0
                && SystemTimeToFileTime(&local, &mut local_file) != 0
        };
        if !done {
            return 0;
        }
        let local_ticks =
            (i64::from(local_file.dwHighDateTime) << 32) | i64::from(local_file.dwLowDateTime);
        ((local_ticks - ticks) / 10_000_000) as i32
    }
}

/// The calendar date and time of `seconds` since 1970 (no time zone applied):
/// (year, month 1–12, day 1–31, hour, minute, second).
pub fn civil(seconds: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400) as u32;
    // Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        year,
        month,
        day,
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil(951_782_400), (2000, 2, 29, 0, 0, 0));
        assert_eq!(civil(1_793_349_045), (2026, 10, 30, 8, 30, 45));
        assert_eq!(civil(-1), (1969, 12, 31, 23, 59, 59));
    }

    #[test]
    fn the_offset_is_within_a_day() {
        let offset = local_utc_offset(1_793_349_045);
        assert!(offset.abs() <= 14 * 3600, "{offset}");
    }
}
